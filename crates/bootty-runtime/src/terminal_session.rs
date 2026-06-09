use std::{
    collections::VecDeque,
    env,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use bootty_surface::geometry::TerminalGeometry;
use bootty_terminal::{
    terminal_engine::{TERMINAL_TERM, TerminalColorConfig, TerminalEngine},
    terminal_frame::RenderFrame,
    terminal_input_model::{KeyInput, MouseInput},
};

pub(crate) const MAX_DRAIN_BYTES_PER_FRAME: usize = 4 * 1024 * 1024;
pub(crate) const MAX_DRAIN_CHUNKS_PER_FRAME: usize = 32;
pub(crate) const MAX_DRAIN_SLICE_BYTES: usize = 256 * 1024;
pub(crate) const MAX_DRAIN_TIME_US: u128 = 20_000;
const MAX_COLLECT_BYTES_PER_TICK: usize = 4 * 1024 * 1024;
const MAX_COLLECT_CHUNKS_PER_TICK: usize = 64;
const MAX_READER_QUEUE_CHUNKS: usize = MAX_COLLECT_CHUNKS_PER_TICK * 2;
const BOOTTY_SHELL_ENV: &str = "BOOTTY_SHELL";
const DEFAULT_SHELL: &str = "/bin/zsh";
pub(crate) const WORKER_READY_FRAME_INTERVAL: Duration = Duration::from_millis(16);
pub(crate) const WORKER_IDLE_SLEEP: Duration = Duration::from_millis(4);
pub(crate) const WORKER_SETTLED_FRAME_DELAY: Duration = Duration::from_millis(16);
pub(crate) const WORKER_MAX_UNPUBLISHED_FRAME_DELAY: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, Default)]
pub struct TerminalSessionConfig {
    pub launch: SessionLaunchConfig,
    pub colors: TerminalColorConfig,
    pub max_scrollback: usize,
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
    geometry: TerminalGeometry,
    pty_master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    tty_name: Option<String>,
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

enum TerminalCommand {
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
        spawn_terminal_worker(TerminalWorkerConfig {
            geometry,
            colors: config.colors,
            max_scrollback: config.max_scrollback,
            pty_rx,
            pty_writer,
            command_rx,
            latest_frame: latest_frame.clone(),
            latest_drain: latest_drain.clone(),
            pending_pty_len: pending_pty_len.clone(),
            repaint_wakeup,
        })?;

        Ok(Self {
            command_tx,
            latest_frame,
            latest_drain,
            pending_pty_len,
            geometry,
            pty_master,
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

    pub fn set_colors(&mut self, colors: TerminalColorConfig) -> Result<()> {
        self.send_command(TerminalCommand::Colors(colors))
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
        self.child
            .try_wait()
            .map(|status| status.is_some())
            .context("poll shell child process")
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
    max_scrollback: usize,
    pty_rx: Receiver<Vec<u8>>,
    pty_writer: Arc<Mutex<Box<dyn Write + Send>>>,
    command_rx: Receiver<TerminalCommand>,
    latest_frame: Arc<PublishedFrame>,
    latest_drain: Arc<Mutex<DrainStats>>,
    pending_pty_len: Arc<AtomicUsize>,
    repaint_wakeup: RepaintWakeup,
}

fn spawn_terminal_worker(config: TerminalWorkerConfig) -> Result<()> {
    let (startup_tx, startup_rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut engine = match TerminalEngine::new_with_scrollback(
            config.geometry,
            config.colors,
            config.max_scrollback,
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
            repaint_wakeup: config.repaint_wakeup,
            output_buf: Vec::with_capacity(1024),
            pending_pty: VecDeque::new(),
            pending_pty_bytes: 0,
            pending_front_offset: 0,
            last_frame_publish: Instant::now() - WORKER_READY_FRAME_INTERVAL,
            unpublished_frame_since: None,
            last_terminal_change: None,
            force_next_frame_publish: false,
            command_disconnected: false,
            pty_disconnected: false,
        };
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
    latest_frame: Arc<PublishedFrame>,
    latest_drain: Arc<Mutex<DrainStats>>,
    pending_pty_len: Arc<AtomicUsize>,
    repaint_wakeup: RepaintWakeup,
    output_buf: Vec<u8>,
    pending_pty: VecDeque<Vec<u8>>,
    pending_pty_bytes: usize,
    pending_front_offset: usize,
    last_frame_publish: Instant,
    unpublished_frame_since: Option<Instant>,
    last_terminal_change: Option<Instant>,
    force_next_frame_publish: bool,
    command_disconnected: bool,
    pty_disconnected: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct WorkerCommandStats {
    did_work: bool,
    terminal_changed: bool,
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
                thread::sleep(WORKER_IDLE_SLEEP);
            }
        }
    }

    fn should_stop(&self) -> bool {
        self.command_disconnected && self.pty_disconnected && self.pending_pty_bytes == 0
    }

    fn should_publish_frame(&self) -> bool {
        should_publish_frame_after_work(
            self.unpublished_frame_since.is_some(),
            self.force_next_frame_publish,
            self.pending_pty_bytes,
            self.last_terminal_change
                .map(|instant| instant.elapsed())
                .unwrap_or(Duration::ZERO),
            self.unpublished_frame_since
                .map(|instant| instant.elapsed())
                .unwrap_or(Duration::ZERO),
        )
    }

    fn mark_unpublished_frame(&mut self) {
        let now = Instant::now();
        self.unpublished_frame_since.get_or_insert(now);
        self.last_terminal_change = Some(now);
    }

    fn mark_input_fast_path(&mut self) {
        self.force_next_frame_publish = true;
    }

    fn process_commands(&mut self) -> WorkerCommandStats {
        let mut stats = WorkerCommandStats::default();
        loop {
            let command = match self.command_rx.try_recv() {
                Ok(command) => command,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.command_disconnected = true;
                    break;
                }
            };
            stats.did_work = true;
            match command {
                TerminalCommand::Resize(geometry) => {
                    self.mark_input_fast_path();
                    if self.engine.resize(geometry).is_ok() {
                        stats.terminal_changed = true;
                    }
                }
                TerminalCommand::Colors(colors) => {
                    if self.engine.set_colors(colors).is_ok() {
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
                } => {
                    self.mark_input_fast_path();
                    match self.engine.is_mouse_tracking() {
                        Ok(true) => {
                            if self
                                .engine
                                .encode_mouse_to_vec(input, &mut self.output_buf)
                                .is_ok()
                            {
                                self.write_output_buf();
                            }
                        }
                        Ok(false) => {
                            self.engine.scroll_viewport_delta(scroll_delta);
                            stats.terminal_changed = true;
                        }
                        Err(_) => {}
                    }
                }
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
            }
        }
        stats
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
            did_work = true;
            collected_bytes += bytes.len();
            collected_chunks += 1;
            self.pending_pty_bytes += bytes.len();
            self.pending_pty.push_back(bytes);
        }
        if did_work {
            self.pending_pty_len
                .store(self.pending_pty_bytes, Ordering::Relaxed);
        }
        did_work
    }

    fn drain_pty(&mut self) -> DrainStats {
        let start = Instant::now();
        let mut stats = DrainStats::default();
        let pending_before = self.pending_pty_bytes;

        while self.pending_pty_bytes > 0
            && !drain_budget_exhausted(stats)
            && !drain_time_exhausted(start)
        {
            let Some(available) = self.front_pending_len() else {
                self.pending_pty_bytes = 0;
                self.pending_front_offset = 0;
                break;
            };
            let consumed = drain_slice_len(stats, available);
            if consumed == 0 {
                break;
            }

            stats.chunks += 1;
            self.write_pending_front(consumed);
            stats.bytes += consumed;
        }

        if self.pending_pty_bytes != pending_before {
            self.pending_pty_len
                .store(self.pending_pty_bytes, Ordering::Relaxed);
        }
        stats.elapsed_us = start.elapsed().as_micros() as u64;
        stats
    }

    fn front_pending_len(&self) -> Option<usize> {
        self.pending_pty
            .front()
            .map(|front| front.len().saturating_sub(self.pending_front_offset))
    }

    fn write_pending_front(&mut self, len: usize) {
        let end = self.pending_front_offset + len;
        if let Some(front) = self.pending_pty.front() {
            self.engine.write_vt(&front[self.pending_front_offset..end]);
        }

        self.pending_front_offset = end;
        self.pending_pty_bytes = self.pending_pty_bytes.saturating_sub(len);
        if self
            .pending_pty
            .front()
            .is_some_and(|front| self.pending_front_offset >= front.len())
        {
            self.pending_pty.pop_front();
            self.pending_front_offset = 0;
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
        if let Ok(frame) = self.engine.extract_frame()
            && self.latest_frame.publish(frame).is_ok()
        {
            self.force_next_frame_publish = false;
            self.unpublished_frame_since = None;
            (self.repaint_wakeup)();
        }
    }

    fn write_output_buf(&self) {
        if !self.output_buf.is_empty() {
            write_pty(&self.pty_writer, &self.output_buf);
        }
    }
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

pub(crate) fn drain_bytes_remaining(stats: DrainStats) -> usize {
    MAX_DRAIN_BYTES_PER_FRAME.saturating_sub(stats.bytes)
}

pub(crate) fn drain_slice_len(stats: DrainStats, available: usize) -> usize {
    drain_bytes_remaining(stats)
        .min(MAX_DRAIN_SLICE_BYTES)
        .min(available)
}

fn drain_time_exhausted(start: Instant) -> bool {
    start.elapsed().as_micros() >= MAX_DRAIN_TIME_US
}

pub(crate) fn drain_budget_exhausted(stats: DrainStats) -> bool {
    stats.bytes >= MAX_DRAIN_BYTES_PER_FRAME || stats.chunks >= MAX_DRAIN_CHUNKS_PER_FRAME
}

pub(crate) fn should_publish_frame_after_work(
    unpublished_frame: bool,
    force_next_frame_publish: bool,
    pending_pty_bytes: usize,
    elapsed_since_last_terminal_change: Duration,
    elapsed_since_first_unpublished: Duration,
) -> bool {
    if !unpublished_frame {
        return false;
    }
    if force_next_frame_publish {
        return true;
    }
    if pending_pty_bytes > 0 {
        return false;
    }
    if elapsed_since_last_terminal_change >= WORKER_SETTLED_FRAME_DELAY {
        return true;
    }
    if elapsed_since_first_unpublished >= WORKER_MAX_UNPUBLISHED_FRAME_DELAY {
        return true;
    }
    false
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
    let mut command = CommandBuilder::new(shell);
    command.args(&config.args);
    command.env("TERM", &config.term);
    command.env("COLORTERM", &config.colorterm);
    for (name, value) in &config.env {
        command.env(name, value);
    }
    for name in &config.env_remove {
        command.env_remove(name);
    }
    if let Some(cwd) = &config.working_directory {
        command.cwd(cwd);
    }

    let tty_name = pair
        .master
        .tty_name()
        .map(|path| path.to_string_lossy().into_owned());

    let child = pair
        .slave
        .spawn_command(command)
        .context("spawn shell in PTY")?;

    let mut reader = pair.master.try_clone_reader()?;
    let writer = Arc::new(Mutex::new(pair.master.take_writer()?));
    let (tx, rx) = mpsc::sync_channel(MAX_READER_QUEUE_CHUNKS);

    thread::spawn(move || {
        let mut buf = [0_u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    Ok((pair.master, writer, rx, child, tty_name))
}

fn shell_command_path(configured: Option<String>) -> String {
    select_shell_path(
        env::var(BOOTTY_SHELL_ENV).ok(),
        configured,
        configured_login_shell(),
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
    fn backlog_catchup_budget_is_large_enough_for_history_bursts() {
        let max_bytes = std::hint::black_box(MAX_DRAIN_BYTES_PER_FRAME);
        let max_slice = std::hint::black_box(MAX_DRAIN_SLICE_BYTES);
        let max_chunks = std::hint::black_box(MAX_DRAIN_CHUNKS_PER_FRAME);
        let max_time = std::hint::black_box(MAX_DRAIN_TIME_US);

        assert!(max_bytes >= 4 * 1024 * 1024);
        assert!(max_slice >= 256 * 1024);
        assert!(max_chunks >= 32);
        assert!(max_time >= 20_000);
    }

    #[test]
    fn worker_frame_publish_policy_avoids_backlog_flicker() {
        assert!(WORKER_READY_FRAME_INTERVAL >= Duration::from_millis(16));
        assert!(WORKER_SETTLED_FRAME_DELAY >= Duration::from_millis(16));
        assert!(WORKER_MAX_UNPUBLISHED_FRAME_DELAY >= Duration::from_millis(500));
        assert!(WORKER_IDLE_SLEEP > Duration::ZERO);
    }

    #[test]
    fn input_wakeup_does_not_publish_stale_pre_echo_frame() {
        assert!(!should_publish_frame_after_work(
            false,
            true,
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
            0,
            Duration::ZERO,
            Duration::ZERO,
        ));
    }

    #[test]
    fn backlog_catchup_does_not_publish_intermediate_frames() {
        assert!(!should_publish_frame_after_work(
            true,
            false,
            4096,
            WORKER_MAX_UNPUBLISHED_FRAME_DELAY * 2,
            WORKER_MAX_UNPUBLISHED_FRAME_DELAY * 2,
        ));
    }

    #[test]
    fn non_input_output_publishes_after_quiet_settle() {
        assert!(should_publish_frame_after_work(
            true,
            false,
            0,
            WORKER_SETTLED_FRAME_DELAY,
            WORKER_SETTLED_FRAME_DELAY,
        ));
    }

    #[test]
    fn continuous_output_has_low_frequency_heartbeat() {
        assert!(should_publish_frame_after_work(
            true,
            false,
            0,
            Duration::ZERO,
            WORKER_MAX_UNPUBLISHED_FRAME_DELAY,
        ));
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
    fn shell_selection_prefers_explicit_then_login_then_environment() {
        assert_eq!(
            select_shell_path(
                Some("/custom/fish".to_string()),
                Some("/configured/bash".to_string()),
                Some("/login/fish".to_string()),
                Some("/env/zsh".to_string()),
            ),
            "/custom/fish",
        );
        assert_eq!(
            select_shell_path(
                Some("relative".to_string()),
                Some("/configured/bash".to_string()),
                Some("/login/fish".to_string()),
                Some("/env/zsh".to_string()),
            ),
            "/configured/bash",
        );
        assert_eq!(
            select_shell_path(
                None,
                Some("relative".to_string()),
                Some("/login/fish".to_string()),
                Some("/env/zsh".to_string()),
            ),
            "/login/fish",
        );
        assert_eq!(
            select_shell_path(
                None,
                Some("".to_string()),
                None,
                Some("/env/zsh".to_string()),
            ),
            "/env/zsh",
        );
        assert_eq!(select_shell_path(None, None, None, None), DEFAULT_SHELL);
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
        fn property_publish_policy_preserves_fast_path_settle_and_backlog_invariants(
            unpublished in any::<bool>(),
            force in any::<bool>(),
            pending_pty_bytes in 0_usize..8192,
            elapsed_change_ms in 0_u64..1000,
            elapsed_unpublished_ms in 0_u64..1000,
        ) {
            let elapsed_change = Duration::from_millis(elapsed_change_ms);
            let elapsed_unpublished = Duration::from_millis(elapsed_unpublished_ms);
            let should_publish = should_publish_frame_after_work(
                unpublished,
                force,
                pending_pty_bytes,
                elapsed_change,
                elapsed_unpublished,
            );

            if !unpublished {
                prop_assert!(!should_publish);
            }
            if unpublished && force {
                prop_assert!(should_publish);
            }
            if unpublished && !force && pending_pty_bytes > 0 {
                prop_assert!(!should_publish);
            }
            if unpublished
                && !force
                && pending_pty_bytes == 0
                && elapsed_change >= WORKER_SETTLED_FRAME_DELAY
            {
                prop_assert!(should_publish);
            }
            if unpublished
                && !force
                && pending_pty_bytes == 0
                && elapsed_unpublished >= WORKER_MAX_UNPUBLISHED_FRAME_DELAY
            {
                prop_assert!(should_publish);
            }
        }
    }
}
