use std::{
    env,
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Command,
    sync::{OnceLock, mpsc},
    thread,
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
    LastWindowRequest, RenameSessionRequest, Request, SwapWindowRequest, WindowTarget,
};
use rmux_sdk::{
    EnsureSession, EnsureSessionPolicy, Rmux, RmuxEndpoint, SessionName,
    SplitDirection as SdkSplitDirection, TerminalSizeSpec, WindowRef,
};
use tokio::runtime::Builder;

use crate::backend::{
    RmuxWindowRow, list_pane_rows, list_window_rows, rmux_request_checked, session_from_rows,
};
use crate::pane_io::{RmuxPaneTarget, pane_for_target};
use bootty_mux::{
    command::{MuxCommand, MuxSplitDirection},
    snapshot::MuxSnapshot,
};

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

struct RmuxBridge {
    snapshot_tx: mpsc::Sender<RmuxSnapshotRequest>,
    control_tx: mpsc::Sender<RmuxControlRequest>,
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

pub(crate) async fn connect_bootty_rmux() -> Result<Rmux> {
    prepare_local_rmux_daemon(bootty_identity::ApplicationIdentity::for_process())?;
    let endpoint = crate::local::endpoint_path().context("resolve Bootty rmux endpoint")?;
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

const BOOTTY_DAEMON_BINARY_ENV: &str = "BOOTTY_DAEMON_BINARY";

pub fn prepare_local_rmux_daemon(identity: bootty_identity::ApplicationIdentity) -> Result<()> {
    identity.initialize_process()?;
    static RESOLVED: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    RESOLVED
        .get_or_init(|| {
            let binary = bootty_daemon_binary().map_err(|error| error.to_string())?;
            // SAFETY: Product composition calls this before rmux workers start. Both values must
            // be visible to the child process created by the rmux SDK.
            unsafe {
                env::set_var(
                    bootty_identity::APPLICATION_IDENTITY_ENV,
                    identity.namespace(),
                );
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
                        bootty_remote::REMOTE_DAEMON_PROTOCOL_VERSION,
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
        thread::spawn(move || run_snapshot_worker(snapshot_rx));
        thread::spawn(move || run_control_worker(control_rx));
        Self {
            snapshot_tx,
            control_tx,
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
            disposition: Default::default(),
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

async fn snapshot_session(
    rmux: &Rmux,
    name: &SessionName,
) -> Result<bootty_mux::snapshot::MuxSession> {
    let session_name = name.to_string();
    let windows = list_window_rows(rmux, name).await?;
    let panes = list_pane_rows(rmux, name).await?;
    Ok(session_from_rows(&session_name, &windows, &panes))
}
