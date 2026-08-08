#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    collections::hash_map::DefaultHasher,
    env,
    ffi::OsStr,
    fs::{File, OpenOptions, TryLockError},
    future::Future,
    hash::{Hash, Hasher},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Command,
    sync::{OnceLock, mpsc},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
#[cfg(feature = "app")]
use bootty_terminal::terminal_engine::{TERMINAL_PROGRAM, TERMINAL_PROGRAM_VERSION, TERMINAL_TERM};
#[cfg(not(feature = "app"))]
const TERMINAL_PROGRAM: &str = "ghostty";
#[cfg(not(feature = "app"))]
const TERMINAL_PROGRAM_VERSION: &str = concat!("Bootty ", env!("CARGO_PKG_VERSION"));
#[cfg(not(feature = "app"))]
const TERMINAL_TERM: &str = "xterm-bootty";
use rmux_proto::{
    LastWindowRequest, PaneTarget, RenameSessionRequest, Request, Response, SwapWindowRequest,
    WindowTarget,
};
use rmux_sdk::{
    EnsureSession, EnsureSessionPolicy, Pane, PaneAttributes, PaneCell, PaneColor, PaneCursor,
    PaneId, PaneOutputChunk, PaneOutputStart, PaneSnapshot, Rmux, RmuxEndpoint, SessionName,
    SplitDirection as SdkSplitDirection, TerminalSizeSpec, WindowRef,
};
use tokio::runtime::Builder;
use tokio::sync::{mpsc as tokio_mpsc, oneshot};

use crate::{
    command::{MuxCommand, MuxSplitDirection},
    rmux::{
        RmuxWindowRow, list_pane_rows, list_window_rows, rmux_request, rmux_request_checked,
        session_from_rows,
    },
    snapshot::MuxSnapshot,
};

const RMUX_OUTPUT_POLL_MIN_DELAY: Duration = Duration::from_millis(1);
const RMUX_OUTPUT_POLL_MAX_DELAY: Duration = Duration::from_millis(16);
const RMUX_RESTORE_CAPTURE_TIMEOUT: Duration = Duration::from_millis(500);
const RMUX_KEYBOARD_PROTOCOL_SCAN_TAIL_BYTES: usize = 64;
const RMUX_KEYBOARD_PROTOCOL_OPTION: &str = "@bootty-keyboard-protocol";
const RMUX_BRACKETED_PASTE_OPTION: &str = "@bootty-bracketed-paste";
const RMUX_MOUSE_MODE_FORMAT: &str = "#{pane_id}\x1f#{mouse_all_flag}\x1f#{mouse_button_flag}\x1f#{mouse_standard_flag}\x1f#{mouse_utf8_flag}\x1f#{mouse_sgr_flag}";

const TERM_ENV: &str = "TERM";
const COLORTERM_ENV: &str = "COLORTERM";
const TERMINFO_ENV: &str = "TERMINFO";
const TERM_PROGRAM_ENV: &str = "TERM_PROGRAM";
const TERM_PROGRAM_VERSION_ENV: &str = "TERM_PROGRAM_VERSION";

fn bootty_rmux_process_environment() -> Vec<String> {
    bootty_rmux_process_environment_with_terminfo(vendored_terminfo_dir())
}

#[cfg(feature = "app")]
fn vendored_terminfo_dir() -> Option<&'static Path> {
    bootty_runtime::terminfo::vendored_terminfo_dir()
}

#[cfg(not(feature = "app"))]
fn vendored_terminfo_dir() -> Option<&'static Path> {
    None
}

fn bootty_rmux_process_environment_with_terminfo(terminfo_dir: Option<&Path>) -> Vec<String> {
    let term = if terminfo_dir.is_some() {
        TERMINAL_TERM
    } else {
        "xterm-256color"
    };
    let mut environment = vec![
        format!("{TERM_ENV}={term}"),
        format!("{COLORTERM_ENV}=truecolor"),
        format!("{TERM_PROGRAM_ENV}={TERMINAL_PROGRAM}"),
        format!("{TERM_PROGRAM_VERSION_ENV}={TERMINAL_PROGRAM_VERSION}"),
    ];
    if let Some(terminfo_dir) = terminfo_dir {
        environment.push(format!("{TERMINFO_ENV}={}", terminfo_dir.to_string_lossy()));
    }
    environment
}

fn apply_bootty_rmux_environment_to_window<'a>(
    mut builder: rmux_sdk::NewWindowBuilder<'a>,
) -> rmux_sdk::NewWindowBuilder<'a> {
    for entry in bootty_rmux_process_environment() {
        if let Some((name, value)) = entry.split_once('=') {
            builder = builder.env(name, value);
        }
    }
    builder
}

fn apply_bootty_rmux_environment_to_split<'a>(
    mut builder: rmux_sdk::PaneSplitBuilder<'a>,
) -> rmux_sdk::PaneSplitBuilder<'a> {
    for entry in bootty_rmux_process_environment() {
        if let Some((name, value)) = entry.split_once('=') {
            builder = builder.env(name, value);
        }
    }
    builder
}

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
    Restore {
        buffered_chunks: Vec<PaneOutputChunk>,
        capture: Vec<u8>,
    },
    Chunks(Vec<PaneOutputChunk>),
    KeyboardProtocol(Vec<u8>),

    Error(String),
}

pub(crate) struct RmuxPaneIo {
    pub(crate) output_rx: mpsc::Receiver<RmuxPaneEvent>,
    pub(crate) input_tx: tokio_mpsc::UnboundedSender<Vec<u8>>,
    pub(crate) resize_tx: tokio_mpsc::UnboundedSender<TerminalSizeSpec>,
    pub(crate) result_rx: mpsc::Receiver<std::result::Result<(), String>>,
}

struct RmuxBridge {
    snapshot_tx: mpsc::Sender<RmuxSnapshotRequest>,
    control_tx: mpsc::Sender<RmuxControlRequest>,
    pane_tx: mpsc::Sender<RmuxOpenPaneRequest>,
}

struct RmuxSnapshotRequest {
    result_tx: mpsc::Sender<std::result::Result<MuxSnapshot, String>>,
}

enum RmuxControlRequest {
    Execute {
        command: MuxCommand,
        result_tx: mpsc::Sender<std::result::Result<(), String>>,
    },
    #[cfg(feature = "app")]
    ResizeWindow {
        window_id: String,
        cols: u16,
        rows: u16,
        result_tx: mpsc::Sender<std::result::Result<(), String>>,
    },
}

struct RmuxOpenPaneRequest {
    target: RmuxPaneTarget,
    max_scrollback: usize,
    output_tx: mpsc::Sender<RmuxPaneEvent>,
    input_rx: tokio_mpsc::UnboundedReceiver<Vec<u8>>,
    resize_rx: tokio_mpsc::UnboundedReceiver<TerminalSizeSpec>,
    result_tx: mpsc::Sender<std::result::Result<(), String>>,
}

struct RmuxBridgeState {
    rmux: Option<Rmux>,
}

pub(crate) fn rmux_snapshot() -> Result<MuxSnapshot> {
    let (result_tx, result_rx) = mpsc::channel();
    bridge()
        .snapshot_tx
        .send(RmuxSnapshotRequest { result_tx })
        .map_err(|_| anyhow::anyhow!("rmux snapshot worker stopped"))?;
    recv_bridge_result(result_rx, "rmux snapshot worker")
}

pub(crate) fn rmux_execute(command: MuxCommand) -> Result<()> {
    request_control_sync(|result_tx| RmuxControlRequest::Execute { command, result_tx })
}

#[cfg(feature = "app")]
pub(crate) fn resize_rmux_window(window_id: &str, cols: u16, rows: u16) -> Result<()> {
    let window_id = window_id.to_owned();
    request_control_sync(|result_tx| RmuxControlRequest::ResizeWindow {
        window_id,
        cols,
        rows,
        result_tx,
    })
}

pub(crate) fn resize_rmux_pane(target: RmuxPaneTarget, size: TerminalSizeSpec) -> Result<()> {
    let io = open_rmux_pane_io(target, 0)?;
    io.resize_tx
        .send(size)
        .map_err(|_| anyhow::anyhow!("rmux pane resize worker stopped"))?;
    recv_bridge_result(io.result_rx, "rmux pane resize worker")
}

pub(crate) fn open_rmux_pane_io(
    target: RmuxPaneTarget,
    max_scrollback: usize,
) -> Result<RmuxPaneIo> {
    let (output_tx, output_rx) = mpsc::channel();
    let (input_tx, input_rx) = tokio_mpsc::unbounded_channel();
    let (resize_tx, resize_rx) = tokio_mpsc::unbounded_channel();
    let (result_tx, result_rx) = mpsc::channel();
    bridge()
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

pub(crate) async fn connect_bootty_rmux() -> Result<Rmux> {
    ensure_rmux_sdk_daemon_binary()?;
    let endpoint = crate::bootty_rmux_endpoint_path().context("resolve Bootty rmux endpoint")?;
    let endpoint = RmuxEndpoint::UnixSocket(endpoint);
    Rmux::builder()
        .endpoint(endpoint)
        .connect_or_start()
        .await
        .map_err(Into::into)
}

pub fn run_embedded_rmux_daemon() -> Result<Option<i32>> {
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    #[cfg(unix)]
    if let Some(code) = rmux_server::run_internal_fifo_reader_helper(arguments.clone()) {
        return Ok(Some(code));
    }
    if arguments
        .first()
        .is_none_or(|argument| argument != rmux_client::INTERNAL_DAEMON_FLAG)
    {
        return Ok(None);
    }
    let socket = arguments
        .get(1)
        .context("rmux daemon invocation omitted endpoint")?;
    let config = rmux_server::DaemonConfig::new(socket.into());
    Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("create embedded rmux daemon runtime")?
        .block_on(async {
            rmux_server::ServerDaemon::new(config)
                .bind()
                .await?
                .wait()
                .await
        })
        .context("run embedded rmux daemon")?;
    Ok(Some(0))
}

#[doc(hidden)]
pub fn start_embedded_rmux_daemon_for_tests() -> Result<()> {
    static STARTED: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    STARTED
        .get_or_init(|| {
            let socket = crate::bootty_rmux_endpoint_path().map_err(|error| error.to_string())?;
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            thread::spawn(move || {
                let started_tx = ready_tx.clone();
                let result = (|| -> Result<()> {
                    let runtime = Builder::new_multi_thread().enable_all().build()?;
                    runtime.block_on(async {
                        let daemon =
                            rmux_server::ServerDaemon::new(rmux_server::DaemonConfig::new(socket))
                                .bind()
                                .await?;
                        let _ = started_tx.send(Ok(()));
                        daemon.wait().await
                    })?;
                    Ok(())
                })();
                if let Err(error) = result {
                    let _ = ready_tx.send(Err(error.to_string()));
                }
            });
            ready_rx.recv().map_err(|error| error.to_string())?
        })
        .clone()
        .map_err(anyhow::Error::msg)
}

const BOOTTY_DAEMON_BINARY_ENV: &str = "BOOTTY_DAEMON_BINARY";

fn ensure_rmux_sdk_daemon_binary() -> Result<()> {
    static RESOLVED: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    RESOLVED
        .get_or_init(|| {
            let binary = bootty_daemon_binary().map_err(|error| error.to_string())?;
            // SAFETY: rmux workers are started only after this one-time initialization.
            unsafe {
                env::set_var(
                    rmux_sdk::bootstrap::discovery::SDK_DAEMON_BINARY_ENV,
                    binary,
                );
            }
            Ok(())
        })
        .clone()
        .map_err(anyhow::Error::msg)
}

fn bootty_daemon_binary() -> Result<PathBuf> {
    let executable = env::current_exe().context("resolve Bootty executable")?;
    Ok(resolve_bootty_daemon_binary(
        &executable,
        env::var_os(BOOTTY_DAEMON_BINARY_ENV).as_deref(),
        sidecar_is_compatible,
    ))
}

fn resolve_bootty_daemon_binary(
    executable: &Path,
    override_binary: Option<&OsStr>,
    is_compatible: impl FnOnce(&Path) -> bool,
) -> PathBuf {
    if let Some(binary) = override_binary {
        return binary.into();
    }
    let daemon = executable.with_file_name(if cfg!(windows) {
        "bootty-daemon.exe"
    } else {
        "bootty-daemon"
    });
    if daemon == executable || (daemon.is_file() && is_compatible(&daemon)) {
        daemon
    } else {
        executable.to_owned()
    }
}

fn sidecar_is_compatible(daemon: &Path) -> bool {
    Command::new(daemon)
        .arg("remote-ping")
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).trim()
                    == format!(
                        "{}:{}",
                        crate::REMOTE_DAEMON_PROTOCOL_VERSION,
                        env!("CARGO_PKG_VERSION")
                    )
        })
}

fn request_control_sync<T>(
    build: impl FnOnce(mpsc::Sender<std::result::Result<T, String>>) -> RmuxControlRequest,
) -> Result<T> {
    let (result_tx, result_rx) = mpsc::channel();
    bridge()
        .control_tx
        .send(build(result_tx))
        .map_err(|_| anyhow::anyhow!("rmux control worker stopped"))?;
    recv_bridge_result(result_rx, "rmux control worker")
}

fn recv_bridge_result<T>(
    result_rx: mpsc::Receiver<std::result::Result<T, String>>,
    worker_name: &str,
) -> Result<T> {
    result_rx
        .recv()
        .map_err(|_| anyhow::anyhow!("{worker_name} stopped"))?
        .map_err(anyhow::Error::msg)
}

fn bridge() -> &'static RmuxBridge {
    static BRIDGE: OnceLock<RmuxBridge> = OnceLock::new();
    BRIDGE.get_or_init(RmuxBridge::start)
}

impl RmuxBridge {
    fn start() -> Self {
        let (snapshot_tx, snapshot_rx) = mpsc::channel();
        let (control_tx, control_rx) = mpsc::channel();
        let (pane_tx, pane_rx) = mpsc::channel();
        thread::spawn(move || run_snapshot_worker(snapshot_rx));
        thread::spawn(move || run_control_worker(control_rx));
        thread::spawn(move || run_pane_worker(pane_rx));
        Self {
            snapshot_tx,
            control_tx,
            pane_tx,
        }
    }
}

fn run_snapshot_worker(request_rx: mpsc::Receiver<RmuxSnapshotRequest>) {
    let runtime = Builder::new_multi_thread()
        .enable_all()
        .thread_name("bootty-rmux-snapshot")
        .worker_threads(1)
        .build()
        .expect("rmux snapshot runtime should initialize");
    let mut state = RmuxBridgeState { rmux: None };
    while let Ok(request) = request_rx.recv() {
        let result = runtime
            .block_on(state.snapshot())
            .map_err(|error| error.to_string());
        let _ = request.result_tx.send(result);
    }
}

fn run_control_worker(request_rx: mpsc::Receiver<RmuxControlRequest>) {
    let runtime = Builder::new_multi_thread()
        .enable_all()
        .thread_name("bootty-rmux-control")
        .worker_threads(1)
        .build()
        .expect("rmux control runtime should initialize");
    let mut state = RmuxBridgeState { rmux: None };
    while let Ok(request) = request_rx.recv() {
        match request {
            RmuxControlRequest::Execute { command, result_tx } => {
                let result = runtime
                    .block_on(state.execute(command))
                    .map_err(|error| error.to_string());
                let _ = result_tx.send(result);
            }
            #[cfg(feature = "app")]
            RmuxControlRequest::ResizeWindow {
                window_id,
                cols,
                rows,
                result_tx,
            } => {
                let result = runtime
                    .block_on(state.resize_window(&window_id, cols, rows))
                    .map_err(|error| error.to_string());
                let _ = result_tx.send(result);
            }
        }
    }
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

impl RmuxBridgeState {
    async fn rmux(&mut self) -> Result<&Rmux> {
        if self.rmux.is_none() {
            self.rmux = Some(connect_bootty_rmux().await?);
        }
        Ok(self.rmux.as_ref().expect("rmux connection initialized"))
    }

    async fn list_session_names(&mut self) -> Result<Vec<SessionName>> {
        let first = {
            let rmux = self.rmux().await?;
            rmux.list_sessions().await
        };
        match first {
            Ok(names) => Ok(names),
            Err(_) => {
                self.rmux = None;
                let rmux = self.rmux().await?;
                rmux.list_sessions().await.map_err(Into::into)
            }
        }
    }

    async fn snapshot(&mut self) -> Result<MuxSnapshot> {
        let first = self.snapshot_current_sessions().await;
        match first {
            Ok(snapshot) => Ok(snapshot),
            Err(error) if should_retry_rmux_error(&error) => {
                self.rmux = None;
                self.snapshot_current_sessions().await
            }
            Err(error) => Err(error),
        }
    }

    async fn snapshot_current_sessions(&mut self) -> Result<MuxSnapshot> {
        let names = self.list_session_names().await?;
        let rmux = self.rmux().await?;
        let mut sessions = Vec::with_capacity(names.len());
        for name in names {
            sessions.push(snapshot_session(rmux, &name).await?);
        }
        Ok(MuxSnapshot {
            active_session_id: sessions
                .iter()
                .find(|session| session.active)
                .map(|session| session.id.clone()),
            sessions,
        })
    }

    async fn execute(&mut self, command: MuxCommand) -> Result<()> {
        let first = self.execute_once(command.clone()).await;
        match first {
            Ok(()) => Ok(()),
            Err(error) if should_retry_rmux_error(&error) => {
                self.rmux = None;
                self.execute_once(command).await
            }
            Err(error) => Err(error),
        }
    }

    async fn execute_once(&mut self, command: MuxCommand) -> Result<()> {
        match command {
            MuxCommand::ActivateWindow {
                session_id,
                window_id,
            } => self.activate_window(&session_id, &window_id).await,
            MuxCommand::CreateProjectSession { session_id, cwd }
            | MuxCommand::CreateWorktreeSession { session_id, cwd } => {
                self.ensure_session(&session_id, &cwd).await
            }
            MuxCommand::RenameSession { session_id, name } => {
                self.rename_session(&session_id, &name).await
            }
            MuxCommand::DitchSession { session_id } => self.kill_session(&session_id).await,
            MuxCommand::RenameWindow {
                session_id,
                window_id,
                name,
            } => self.rename_window(&session_id, &window_id, &name).await,
            MuxCommand::NewWindow { session_id, cwd } => {
                self.new_window(&session_id, cwd.as_deref()).await
            }
            MuxCommand::ActivateNextWindow { session_id } => {
                self.activate_relative_window(&session_id, 1).await
            }
            MuxCommand::ActivatePreviousWindow { session_id } => {
                self.activate_relative_window(&session_id, -1).await
            }
            MuxCommand::ActivateLastWindow { session_id } => {
                self.activate_last_window(&session_id).await
            }
            MuxCommand::ActivateWindowIndex { session_id, index } => {
                self.activate_window_index(&session_id, index).await
            }
            MuxCommand::MoveWindow {
                session_id,
                window_id,
                delta,
            } => {
                self.move_window(&session_id, window_id.as_deref(), delta)
                    .await
            }
            MuxCommand::MoveWindowPreservingSelection {
                session_id,
                window_id,
                delta,
                selected_window_id,
            } => {
                self.move_window(&session_id, Some(&window_id), delta)
                    .await?;
                self.activate_window(&session_id, &selected_window_id).await
            }
            MuxCommand::SplitPane {
                session_id,
                pane_id,
                direction,
            } => {
                self.split_pane(&session_id, pane_id.as_deref(), direction)
                    .await
            }
            MuxCommand::KillPane {
                session_id,
                pane_id,
            }
            | MuxCommand::ClosePane {
                session_id,
                pane_id,
            } => self.close_pane(&session_id, pane_id.as_deref()).await,
            MuxCommand::SelectPane { .. }
            | MuxCommand::SelectNextPane { .. }
            | MuxCommand::SelectPreviousPane { .. }
            | MuxCommand::TogglePaneZoom { .. } => {
                anyhow::bail!("rmux backend does not support mux command {command:?}")
            }
        }?;
        Ok(())
    }

    async fn ensure_session(&mut self, session_name: &str, cwd: &str) -> Result<()> {
        let rmux = self.rmux().await?;
        let name = SessionName::new(session_name).context("invalid rmux session name")?;
        rmux.ensure_session(
            EnsureSession::named(name)
                .policy(EnsureSessionPolicy::CreateOrReuse)
                .detached(true)
                .working_directory(cwd)
                .size(TerminalSizeSpec::new(80, 24))
                .environment(bootty_rmux_process_environment()),
        )
        .await?;
        Ok(())
    }

    async fn rename_session(&mut self, session_name: &str, name: &str) -> Result<()> {
        self.rmux().await?;
        rmux_request_checked(Request::RenameSession(RenameSessionRequest {
            target: SessionName::new(session_name).context("invalid rmux session name")?,
            new_name: SessionName::new(name).context("invalid rmux session name")?,
        }))
        .await
    }

    async fn kill_session(&mut self, session_name: &str) -> Result<()> {
        let rmux = self.rmux().await?;
        let name = SessionName::new(session_name).context("invalid rmux session name")?;
        rmux.session(name).await?.kill().await?;
        Ok(())
    }

    async fn activate_window(&mut self, session_name: &str, window_id: &str) -> Result<()> {
        let Some((session_name, index)) = self.window_index_by_id(session_name, window_id).await?
        else {
            anyhow::bail!("rmux window {window_id} not found in session {session_name}");
        };
        self.window(&session_name, index).await?.select().await?;
        Ok(())
    }

    async fn rename_window(
        &mut self,
        session_name: &str,
        window_id: &str,
        name: &str,
    ) -> Result<()> {
        let Some((session_name, index)) = self.window_index_by_id(session_name, window_id).await?
        else {
            anyhow::bail!("rmux window {window_id} not found in session {session_name}");
        };
        self.window(&session_name, index)
            .await?
            .rename(name)
            .await?;
        Ok(())
    }

    async fn new_window(&mut self, session_name: &str, cwd: Option<&str>) -> Result<()> {
        let rmux = self.rmux().await?;
        let name = SessionName::new(session_name).context("invalid rmux session name")?;
        let window_index = append_window_index(&list_window_rows(rmux, &name).await?);
        let session = rmux.session(name).await?;
        let mut builder = apply_bootty_rmux_environment_to_window(
            session.new_window_with().at_index(window_index),
        );
        if let Some(cwd) = cwd {
            builder = builder.cwd(cwd);
        }
        builder.await?;
        Ok(())
    }

    async fn activate_relative_window(&mut self, session_name: &str, delta: i32) -> Result<()> {
        let rows = self.window_rows(session_name).await?;
        if rows.is_empty() {
            return Ok(());
        }
        let current = rows.iter().position(|window| window.active).unwrap_or(0);
        let next = (current as i32 + delta).rem_euclid(rows.len() as i32) as usize;
        self.window(session_name, rows[next].index)
            .await?
            .select()
            .await?;
        Ok(())
    }

    async fn activate_last_window(&mut self, session_name: &str) -> Result<()> {
        self.rmux().await?;
        rmux_request_checked(Request::LastWindow(LastWindowRequest {
            target: SessionName::new(session_name).context("invalid rmux session name")?,
        }))
        .await
    }

    async fn activate_window_index(&mut self, session_name: &str, index: u32) -> Result<()> {
        let rows = self.window_rows(session_name).await?;
        let Some(window) = rows
            .iter()
            .find(|window| display_window_index(&rows, window) == index)
        else {
            return Ok(());
        };
        self.window(session_name, window.index)
            .await?
            .select()
            .await?;
        Ok(())
    }

    async fn move_window(
        &mut self,
        session_name: &str,
        window_id: Option<&str>,
        delta: i32,
    ) -> Result<()> {
        let rows = self.window_rows(session_name).await?;
        let source = window_id
            .and_then(|window_id| rows.iter().position(|window| window.id == window_id))
            .or_else(|| rows.iter().position(|window| window.active))
            .context("rmux move window requires an active target")?;
        let target = (source as i32 + delta).clamp(0, rows.len() as i32 - 1) as usize;
        if source == target {
            return Ok(());
        }
        self.rmux().await?;
        let session = SessionName::new(session_name).context("invalid rmux session name")?;
        rmux_request_checked(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(session.clone(), rows[source].index),
            target: WindowTarget::with_window(session, rows[target].index),
            detached: true,
        }))
        .await?;
        Ok(())
    }

    async fn split_pane(
        &mut self,
        session_name: &str,
        pane_id: Option<&str>,
        direction: MuxSplitDirection,
    ) -> Result<()> {
        let rmux = self.rmux().await?;
        let pane = pane_for_target(
            rmux,
            &RmuxPaneTarget::new(session_name, pane_id.map(str::to_owned)),
        )
        .await?;
        let direction = match direction {
            MuxSplitDirection::Right => SdkSplitDirection::Right,
            MuxSplitDirection::Down => SdkSplitDirection::Down,
        };
        apply_bootty_rmux_environment_to_split(pane.split_with(direction)).await?;
        Ok(())
    }

    async fn close_pane(&mut self, session_name: &str, pane_id: Option<&str>) -> Result<()> {
        let pane_id = pane_id.context("rmux close pane requires a focused pane id")?;
        let rmux = self.rmux().await?;
        pane_for_target(
            rmux,
            &RmuxPaneTarget::new(session_name, Some(pane_id.to_owned())),
        )
        .await?
        .close()
        .await?;
        Ok(())
    }

    #[cfg(feature = "app")]
    async fn resize_window(&mut self, window_id: &str, cols: u16, rows: u16) -> Result<()> {
        let first = self.resize_window_once(window_id, cols, rows).await;
        match first {
            Ok(()) => Ok(()),
            Err(error) if should_retry_rmux_error(&error) => {
                self.rmux = None;
                self.resize_window_once(window_id, cols, rows).await
            }
            Err(error) => Err(error),
        }
    }

    #[cfg(feature = "app")]
    async fn resize_window_once(&mut self, window_id: &str, cols: u16, rows: u16) -> Result<()> {
        let Some((session_name, index)) = self.any_window_index_by_id(window_id).await? else {
            anyhow::bail!("rmux window {window_id} not found");
        };
        self.window(&session_name, index)
            .await?
            .resize(Some(cols), Some(rows))
            .await?;
        Ok(())
    }

    #[cfg(feature = "app")]
    async fn any_window_index_by_id(&mut self, window_id: &str) -> Result<Option<(String, u32)>> {
        let names = self.list_session_names().await?;
        let rmux = self.rmux().await?;
        for name in names {
            let rows = list_window_rows(rmux, &name).await?;
            if let Some(row) = rows.iter().find(|row| row.id == window_id) {
                return Ok(Some((row.session_name.clone(), row.index)));
            }
        }
        Ok(None)
    }

    async fn window_index_by_id(
        &mut self,
        session_name: &str,
        window_id: &str,
    ) -> Result<Option<(String, u32)>> {
        let rows = self.window_rows(session_name).await?;
        Ok(rows
            .iter()
            .find(|row| row.id == window_id)
            .map(|row| (row.session_name.clone(), row.index)))
    }

    async fn window_rows(&mut self, session_name: &str) -> Result<Vec<RmuxWindowRow>> {
        let rmux = self.rmux().await?;
        let name = SessionName::new(session_name).context("invalid rmux session name")?;
        list_window_rows(rmux, &name).await
    }

    async fn window(&mut self, session_name: &str, index: u32) -> Result<rmux_sdk::Window> {
        let rmux = self.rmux().await?;
        let name = SessionName::new(session_name).context("invalid rmux session name")?;
        rmux.window(WindowRef::new(name, index))
            .await
            .map_err(Into::into)
    }
}

fn should_retry_rmux_error(error: &anyhow::Error) -> bool {
    let text = error.to_string();
    text.contains("transport")
        || text.contains("closed the transport")
        || text.contains("connection refused")
        || text.contains("No such file")
}

fn append_window_index(rows: &[RmuxWindowRow]) -> u32 {
    rows.iter()
        .map(|window| window.index)
        .max()
        .map_or(0, |index| index.saturating_add(1))
}

fn display_window_index(rows: &[RmuxWindowRow], row: &RmuxWindowRow) -> u32 {
    let mut ordered = rows.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        left.index
            .cmp(&right.index)
            .then_with(|| left.id.cmp(&right.id))
    });
    ordered
        .iter()
        .position(|candidate| candidate.session_name == row.session_name && candidate.id == row.id)
        .map(|position| position as u32 + 1)
        .unwrap_or(row.index)
}

async fn snapshot_session(rmux: &Rmux, name: &SessionName) -> Result<crate::snapshot::MuxSession> {
    let session_name = name.to_string();
    let windows = list_window_rows(rmux, name).await?;
    let panes = list_pane_rows(rmux, name).await?;
    Ok(session_from_rows(&session_name, &windows, &panes))
}

async fn run_pane_io(
    target: RmuxPaneTarget,
    max_scrollback: usize,
    output_tx: mpsc::Sender<RmuxPaneEvent>,
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
        let _ = output_tx.send(RmuxPaneEvent::Error(text));
    }
}

async fn replay_retained_terminal_protocol(
    pane: &Pane,
    output_tx: &mpsc::Sender<RmuxPaneEvent>,
    mouse_modes: &[u16],
) -> Result<()> {
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
    if !protocol.is_empty() {
        let _ = output_tx.send(RmuxPaneEvent::KeyboardProtocol(protocol));
    }
    Ok(())
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

fn send_restored_output(
    capture: Option<Vec<u8>>,
    output_tx: &mpsc::Sender<RmuxPaneEvent>,
    buffered_chunks: &mut Vec<PaneOutputChunk>,
) -> bool {
    output_tx
        .send(RmuxPaneEvent::Restore {
            buffered_chunks: std::mem::take(buffered_chunks),
            capture: capture.unwrap_or_default(),
        })
        .is_ok()
}

struct RmuxLiveOutput {
    file: File,
    reader_lock: File,
    endpoint: PathBuf,
    pipe_target: PaneTarget,
    path: PathBuf,
    init_lock_path: PathBuf,
    sequence: u64,
}

impl RmuxLiveOutput {
    async fn open(rmux: &Rmux, target: &RmuxPaneTarget) -> Result<Self> {
        let pipe_target = pane_pipe_target(rmux, target).await?;
        let endpoint = crate::bootty_rmux_endpoint_path()?;
        let (path, init_lock_path, reader_lock_path) = rmux_pipe_paths(&endpoint, &pipe_target);
        let init_lock = open_private_file(&init_lock_path)?;
        init_lock
            .lock()
            .context("lock rmux output initialization")?;
        let reader_lock = open_private_file(&reader_lock_path)?;
        let first_reader = match reader_lock.try_lock() {
            Ok(()) => true,
            Err(TryLockError::WouldBlock) => false,
            Err(TryLockError::Error(error)) => {
                return Err(error).context("lock rmux output readers");
            }
        };

        let mut file = open_private_file(&path)?;
        if first_reader {
            file.set_len(0)?;
            file.seek(SeekFrom::Start(0))?;
            set_rmux_pipe(
                &endpoint,
                &pipe_target,
                Some(format!(
                    "cat >> {}",
                    crate::tmux_protocol::shell_quote(&path.to_string_lossy())
                )),
            )?;
            reader_lock.unlock()?;
        } else {
            file.seek(SeekFrom::End(0))?;
        }
        reader_lock
            .lock_shared()
            .context("share rmux output reader lock")?;
        init_lock.unlock()?;

        Ok(Self {
            file,
            reader_lock,
            endpoint,
            pipe_target,
            path,
            init_lock_path,
            sequence: 0,
        })
    }

    fn poll_once(&mut self) -> Result<Vec<PaneOutputChunk>> {
        let mut bytes = Vec::new();
        self.file.read_to_end(&mut bytes)?;
        if bytes.is_empty() {
            return Ok(Vec::new());
        }
        let sequence = self.sequence;
        self.sequence = self.sequence.saturating_add(1);
        Ok(vec![PaneOutputChunk::Bytes { sequence, bytes }])
    }
}

impl Drop for RmuxLiveOutput {
    fn drop(&mut self) {
        let Ok(init_lock) = open_private_file(&self.init_lock_path) else {
            return;
        };
        if init_lock.lock().is_err() {
            return;
        }
        let _ = self.reader_lock.unlock();
        if self.reader_lock.try_lock().is_ok() {
            let _ = set_rmux_pipe(&self.endpoint, &self.pipe_target, None);
            let _ = std::fs::remove_file(&self.path);
            let _ = self.reader_lock.unlock();
        }
        let _ = init_lock.unlock();
    }
}

fn open_private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
        .open(path)
        .with_context(|| format!("open rmux output file {}", path.display()))
}

fn set_rmux_pipe(endpoint: &Path, target: &PaneTarget, command: Option<String>) -> Result<()> {
    match rmux_client::connect(endpoint)?.pipe_pane(target.clone(), false, true, false, command)? {
        Response::PipePane(_) => Ok(()),
        Response::Error(error) => Err(anyhow::anyhow!(error.error)),
        response => anyhow::bail!("unexpected rmux pipe-pane response: {response:?}"),
    }
}

fn rmux_pipe_paths(endpoint: &Path, target: &PaneTarget) -> (PathBuf, PathBuf, PathBuf) {
    let mut hasher = DefaultHasher::new();
    endpoint.hash(&mut hasher);
    target.hash(&mut hasher);
    let id = hasher.finish();
    let root = endpoint.parent().unwrap_or_else(|| Path::new("."));
    (
        root.join(format!("bootty-rmux-output-{id:016x}")),
        root.join(format!("bootty-rmux-output-{id:016x}.init.lock")),
        root.join(format!("bootty-rmux-output-{id:016x}.readers.lock")),
    )
}

async fn pane_pipe_target(rmux: &Rmux, target: &RmuxPaneTarget) -> Result<PaneTarget> {
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
    output_tx: &mpsc::Sender<RmuxPaneEvent>,
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
            let _ = output_tx.send(RmuxPaneEvent::KeyboardProtocol(protocol));
        }
    } else {
        replay_retained_terminal_protocol(&pane, output_tx, &mouse_modes).await?;
    }
    let mut live_output = RmuxLiveOutput::open(&rmux, &target).await?;
    let input_target = live_output.pipe_target.clone();
    let mut restore_rx = start_restore_capture(target.clone(), max_scrollback);
    let mut restore_pending = true;
    let mut buffered_chunks = Vec::new();
    let mut output_poll_delay = RMUX_OUTPUT_POLL_MIN_DELAY;
    let mut terminal_protocol_tail = Vec::new();

    loop {
        tokio::select! {
            restore = &mut restore_rx, if restore_pending => {
                restore_pending = false;
                if !send_restored_output(
                    restore.ok().flatten(),
                    output_tx,
                    &mut buffered_chunks,
                ) {
                    break;
                }
            }
            _ = tokio::time::sleep(output_poll_delay) => {
                let chunks = live_output.poll_once()?;
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
                    if restore_pending {
                        buffered_chunks.extend(chunks);
                    } else if output_tx.send(RmuxPaneEvent::Chunks(chunks)).is_err() {
                        break;
                    }
                }
            }
            Some(mut bytes) = input_rx.recv() => {
                while let Ok(next) = input_rx.try_recv() {
                    bytes.extend_from_slice(&next);
                }
                if restore_pending {
                    restore_pending = false;
                    let capture = (&mut restore_rx).await.ok().flatten();
                    if !send_restored_output(capture, output_tx, &mut buffered_chunks) {
                        break;
                    }
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

async fn pane_for_target(rmux: &Rmux, target: &RmuxPaneTarget) -> Result<Pane> {
    let session_name = target.session_name()?;
    if let Some(pane_id) = target.pane_id() {
        return Ok(rmux.pane_by_id(session_name, pane_id).await?);
    }
    Ok(rmux.session(session_name).await?.pane(0, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::Path,
        thread,
        time::{Duration, Instant},
    };

    #[test]
    fn packaged_app_uses_its_daemon_sidecar() {
        let directory = tempfile::tempdir().expect("tempdir");
        let executable = directory.path().join(if cfg!(windows) {
            "bootty.exe"
        } else {
            "bootty"
        });
        let daemon = directory.path().join(if cfg!(windows) {
            "bootty-daemon.exe"
        } else {
            "bootty-daemon"
        });
        std::fs::write(&daemon, []).expect("daemon");

        assert_eq!(
            resolve_bootty_daemon_binary(&executable, None, |candidate| candidate == daemon),
            daemon
        );
    }

    #[test]
    fn stale_sidecar_falls_back_to_the_app_daemon() {
        let directory = tempfile::tempdir().expect("tempdir");
        let executable = directory.path().join("bootty");
        std::fs::write(directory.path().join("bootty-daemon"), []).expect("daemon");

        assert_eq!(
            resolve_bootty_daemon_binary(&executable, None, |_| false),
            executable
        );
    }

    #[test]
    fn stalled_restore_capture_completes_without_history() {
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();

        let capture = runtime.block_on(complete_restore_capture(
            Duration::from_millis(1),
            std::future::pending::<Option<Vec<u8>>>(),
        ));

        assert_eq!(capture, None);
    }

    #[test]
    fn display_window_index_uses_compact_visual_order_for_skipped_rmux_indexes() {
        let rows = vec![
            RmuxWindowRow {
                session_name: "alpha".to_owned(),
                id: "@10".to_owned(),
                index: 0,
                active: false,
                name: "one".to_owned(),
                layout: None,
            },
            RmuxWindowRow {
                session_name: "alpha".to_owned(),
                id: "@12".to_owned(),
                index: 2,
                active: true,
                name: "three".to_owned(),
                layout: None,
            },
        ];

        assert_eq!(display_window_index(&rows, &rows[1]), 2);
    }

    #[test]
    fn new_window_index_appends_after_gaps() {
        let rows = vec![
            RmuxWindowRow {
                session_name: "alpha".to_owned(),
                id: "@10".to_owned(),
                index: 0,
                active: false,
                name: "one".to_owned(),
                layout: None,
            },
            RmuxWindowRow {
                session_name: "alpha".to_owned(),
                id: "@12".to_owned(),
                index: 2,
                active: true,
                name: "three".to_owned(),
                layout: None,
            },
        ];

        assert_eq!(append_window_index(&rows), 3);
    }

    #[test]
    fn rmux_process_environment_advertises_bootty_terminal_identity() {
        let environment =
            bootty_rmux_process_environment_with_terminfo(Some(Path::new("/bootty/terminfo")));

        assert_eq!(
            environment,
            vec![
                "TERM=xterm-bootty".to_owned(),
                "COLORTERM=truecolor".to_owned(),
                "TERM_PROGRAM=ghostty".to_owned(),
                format!("TERM_PROGRAM_VERSION={TERMINAL_PROGRAM_VERSION}"),
                "TERMINFO=/bootty/terminfo".to_owned(),
            ]
        );
    }

    #[test]
    fn rmux_process_environment_falls_back_without_bootty_terminfo() {
        let environment = bootty_rmux_process_environment_with_terminfo(None);

        assert_eq!(
            environment,
            vec![
                "TERM=xterm-256color".to_owned(),
                "COLORTERM=truecolor".to_owned(),
                "TERM_PROGRAM=ghostty".to_owned(),
                format!("TERM_PROGRAM_VERSION={TERMINAL_PROGRAM_VERSION}"),
            ]
        );
    }

    #[test]
    fn restore_cursor_sequence_uses_one_based_terminal_coordinates() {
        let mut bytes = b"screen".to_vec();

        append_restore_cursor_position(&mut bytes, PaneCursor::new(7, 14, true, 0));

        assert_eq!(bytes, b"screen\x1b[8;15H\x1b[?25h");
    }

    #[test]
    fn restore_cursor_sequence_can_hide_cursor() {
        let mut bytes = Vec::new();

        append_restore_cursor_position(&mut bytes, PaneCursor::new(0, 0, false, 0));

        assert_eq!(bytes, b"\x1b[1;1H\x1b[?25l");
    }

    #[test]
    fn restore_snapshot_visible_rewrites_screen_before_cursor() {
        let snapshot = PaneSnapshot::new(
            4,
            2,
            vec![
                rmux_sdk::PaneCell::new(rmux_sdk::PaneGlyph::new("a", 1)),
                rmux_sdk::PaneCell::new(rmux_sdk::PaneGlyph::new("b", 1)),
                rmux_sdk::PaneCell::blank(),
                rmux_sdk::PaneCell::blank(),
                rmux_sdk::PaneCell::new(rmux_sdk::PaneGlyph::new("c", 1)),
                rmux_sdk::PaneCell::new(rmux_sdk::PaneGlyph::new("d", 1)),
                rmux_sdk::PaneCell::new(rmux_sdk::PaneGlyph::new("e", 1)),
                rmux_sdk::PaneCell::blank(),
            ],
            PaneCursor::new(1, 2, true, 0),
        )
        .unwrap();
        let mut bytes = b"history\r\n".to_vec();

        append_restore_snapshot_visible(&mut bytes, &snapshot);

        assert_eq!(
            bytes,
            b"history\r\n\x1b[?25l\x1b[H\x1b[J\x1b[1;1H\x1b[0ma\x1b[1;2H\x1b[0mb\x1b[2;1H\x1b[0mc\x1b[2;2H\x1b[0md\x1b[2;3H\x1b[0me\x1b[0m\x1b[2;3H\x1b[?25h"
        );
    }
    #[test]
    fn restore_snapshot_realigns_hyperlinked_capture_before_cursor() {
        let snapshot = PaneSnapshot::new(
            4,
            1,
            vec![
                rmux_sdk::PaneCell::new(rmux_sdk::PaneGlyph::new("l", 1)),
                rmux_sdk::PaneCell::new(rmux_sdk::PaneGlyph::new("i", 1)),
                rmux_sdk::PaneCell::new(rmux_sdk::PaneGlyph::new("n", 1)),
                rmux_sdk::PaneCell::new(rmux_sdk::PaneGlyph::new("k", 1)),
            ],
            PaneCursor::new(0, 3, true, 0),
        )
        .unwrap();
        let mut bytes = b"\x1b]8;;file:///tmp/example.png\x1b\\link\x1b]8;;\x1b\\".to_vec();

        append_restore_snapshot(&mut bytes, &snapshot);

        assert!(bytes.windows(4).any(|window| window == b"\x1b]8;"));
        assert!(bytes.windows(6).any(|window| window == b"\x1b[H\x1b[J"));
        assert!(bytes.ends_with(b"\x1b[1;4H\x1b[?25h"));
    }

    #[test]
    fn restore_snapshot_visible_preserves_cell_color_and_attributes() {
        let mut styled = PaneCell::new(rmux_sdk::PaneGlyph::new("x", 1));
        styled.attributes = PaneAttributes::BOLD | PaneAttributes::UNDERLINE;
        styled.foreground = PaneColor::Rgb {
            red: 1,
            green: 2,
            blue: 3,
        };
        styled.background = PaneColor::Indexed { index: 4 };
        styled.underline = PaneColor::BrightAnsi { index: 2 };
        let mut blank_background = PaneCell::blank();
        blank_background.background = PaneColor::Ansi { index: 1 };
        let snapshot = PaneSnapshot::new(
            2,
            1,
            vec![styled, blank_background],
            PaneCursor::new(0, 1, true, 0),
        )
        .unwrap();
        let mut bytes = Vec::new();

        append_restore_snapshot_visible(&mut bytes, &snapshot);

        assert_eq!(
            bytes,
            b"\x1b[?25l\x1b[H\x1b[J\x1b[1;1H\x1b[0;1;4;38;2;1;2;3;48;5;4;58;5;10mx\x1b[1;2H\x1b[0;41m \x1b[0m\x1b[1;2H\x1b[?25h"
        );
    }

    #[test]
    fn retained_terminal_protocol_is_replayed_without_screen_output() {
        assert_eq!(
            kitty_keyboard_protocol_query(b"prompt\x1b[>7u\x1b[?u\x1b[c"),
            Some(b"\x1b[>7u\x1b[?u".to_vec())
        );
        assert_eq!(
            kitty_keyboard_protocol_flags(b"\x1b[>7u\x1b[?u"),
            Some("7".to_owned())
        );
        assert_eq!(
            restored_terminal_protocol(Some("7"), true, &[1000, 1006]),
            b"\x1b[>7u\x1b[?2004h\x1b[?1000h\x1b[?1006h".to_vec()
        );
        assert_eq!(bracketed_paste_mode(b"\x1b[?2004h"), Some(true));
        assert_eq!(bracketed_paste_mode(b"\x1b[?2004h\x1b[?2004l"), Some(false));
        assert_eq!(kitty_keyboard_protocol_query(b"\x1b[>7u\x1b[?"), None);
        assert_eq!(kitty_keyboard_protocol_query(b"\x1b[>xu\x1b[?u"), None);
        assert_eq!(
            parse_rmux_mouse_protocol_modes("%152\x1f1\x1f0\x1f0\x1f0\x1f1"),
            Some(("%152", vec![1003, 1006]))
        );
        assert_eq!(
            parse_rmux_mouse_protocol_modes("%152\x1f0\x1f0\x1f1\x1f0\x1f0"),
            Some(("%152", vec![1000]))
        );
    }

    #[test]
    fn rmux_hex_keys_preserve_arbitrary_input_bytes() {
        assert_eq!(rmux_hex_keys(&[0x1b, 0x80, 0xff]), ["1b", "80", "ff"]);
    }

    #[test]
    fn restored_output_detects_disconnected_receiver_without_chunks() {
        let (output_tx, output_rx) = mpsc::channel();
        drop(output_rx);
        let mut buffered_chunks = Vec::new();

        assert!(!send_restored_output(
            None,
            &output_tx,
            &mut buffered_chunks
        ));
    }

    #[test]
    fn restored_output_applies_capture_after_buffered_live_output() {
        let (output_tx, output_rx) = mpsc::channel();
        let mut buffered_chunks = vec![PaneOutputChunk::Bytes {
            sequence: 0,
            bytes: b"duplicate line\r\n".to_vec(),
        }];

        assert!(send_restored_output(
            Some(b"\x1b[H\x1b[Jduplicate line".to_vec()),
            &output_tx,
            &mut buffered_chunks,
        ));
        let RmuxPaneEvent::Restore {
            buffered_chunks,
            capture,
        } = output_rx.recv().unwrap()
        else {
            panic!("expected restore event");
        };
        assert_eq!(buffered_chunks.len(), 1);
        assert_eq!(capture, b"\x1b[H\x1b[Jduplicate line");
    }

    #[test]
    #[ignore = "requires an isolated RMUX_TMPDIR"]
    fn rmux_live_control_commands_do_not_wait_for_background_snapshot() -> Result<()> {
        std::env::var_os("RMUX_TMPDIR").context("set isolated RMUX_TMPDIR")?;
        start_embedded_rmux_daemon_for_tests()?;

        let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
        let sessions = (0..10)
            .map(|index| format!("bootty-bridge-priority-{}-{index}", std::process::id()))
            .collect::<Vec<_>>();
        for session_id in &sessions {
            rmux_execute(MuxCommand::CreateProjectSession {
                session_id: session_id.clone(),
                cwd: cwd.clone(),
            })?;
            for _ in 0..3 {
                rmux_execute(MuxCommand::NewWindow {
                    session_id: session_id.clone(),
                    cwd: Some(cwd.clone()),
                })?;
            }
        }

        let _ = rmux_snapshot()?;
        let snapshot_thread = thread::spawn(rmux_snapshot);
        thread::sleep(Duration::from_millis(5));

        let start = Instant::now();
        rmux_execute(MuxCommand::ActivateNextWindow {
            session_id: sessions[0].clone(),
        })?;
        let elapsed = start.elapsed();

        eprintln!("rmux bridge priority probe: activate while snapshot pending = {elapsed:?}");
        assert!(
            elapsed < Duration::from_millis(250),
            "rmux control command should not queue behind background snapshot: {elapsed:?}"
        );

        let _ = snapshot_thread
            .join()
            .expect("snapshot thread should not panic");
        for session_id in sessions {
            let _ = rmux_execute(MuxCommand::DitchSession { session_id });
        }
        Ok(())
    }
}
