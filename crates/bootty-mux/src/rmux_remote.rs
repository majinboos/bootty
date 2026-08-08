//! Runs Bootty's embedded rmux backend through the small remote Bootty daemon.
//!
//! The remote host never resolves or executes an `rmux` binary. Bootty serializes backend requests,
//! sends them through SSH, and handles them with the same embedded rmux SDK path used locally.

#[cfg(feature = "app")]
use std::io::BufReader;
use std::io::{BufRead, BufWriter, Write};
use std::thread;
#[cfg(feature = "app")]
use std::{
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
};

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rmux_sdk::{PaneOutputChunk, TerminalSizeSpec};
use serde::{Deserialize, Serialize};
#[cfg(feature = "app")]
use tokio::sync::mpsc as tokio_mpsc;

#[cfg(feature = "app")]
use crate::{
    backend::MuxBackend,
    capability::BindingCapabilityDescriptor,
    controller::MuxScope,
    process::{CommandOutput, CommandRunner, SystemCommandRunner},
    rmux::rmux_capabilities,
    rmux_bridge::RmuxPaneIo,
    snapshot::MuxSnapshot,
    ssh::{SshRemote, remote_daemon_failure},
};
use crate::{
    command::MuxCommand,
    rmux_bridge::{
        RmuxPaneEvent, RmuxPaneTarget, open_rmux_pane_io, resize_rmux_pane, rmux_execute,
        rmux_snapshot,
    },
};

#[cfg(feature = "app")]
const REMOTE_RMUX_SUBCOMMAND: &str = "remote-rmux";
const MAX_REMOTE_RMUX_PAYLOAD: usize = 1024 * 1024;

#[derive(Debug, Deserialize, Serialize)]
enum RemoteRmuxRequest {
    Snapshot,
    Execute {
        command: MuxCommand,
    },
    PaneStream {
        session: String,
        pane: String,
        max_scrollback: usize,
    },
    PaneInput {
        session: String,
        pane: String,
    },
    Resize {
        session: String,
        pane: String,
        cols: u16,
        rows: u16,
    },
}

#[derive(Debug, Deserialize, Serialize)]
enum RemotePaneFrame {
    Restore {
        capture: String,
        buffered_chunks: Vec<String>,
    },
    Chunks(Vec<String>),
    KeyboardProtocol(String),
    Error(String),
}

#[cfg(feature = "app")]
pub struct RemoteRmuxBackend {
    remote: SshRemote,
}

#[cfg(feature = "app")]
impl RemoteRmuxBackend {
    pub fn new(remote: SshRemote) -> Self {
        Self { remote }
    }

    fn run(&self, request: &RemoteRmuxRequest) -> Result<CommandOutput> {
        self.remote.ensure_daemon()?;
        let (program, args) = remote_rmux_argv(&self.remote, request)?;
        let output = SystemCommandRunner.run(&program, &args)?;
        if output.success {
            return Ok(output);
        }
        bail!(
            "{}",
            remote_daemon_failure(self.remote.host(), &output.stderr)
        )
    }
}

#[cfg(feature = "app")]
impl MuxBackend for RemoteRmuxBackend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        let output = self.run(&RemoteRmuxRequest::Snapshot)?;
        serde_json::from_str(&output.stdout).context("decode remote Space snapshot")
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        self.run(&RemoteRmuxRequest::Execute { command })?;
        Ok(())
    }

    fn capabilities(&self, scope: MuxScope) -> BindingCapabilityDescriptor {
        rmux_capabilities(scope)
    }
}

#[cfg(feature = "app")]
pub(crate) fn open_remote_rmux_pane_io(
    remote: &SshRemote,
    target: &RmuxPaneTarget,
    max_scrollback: usize,
) -> Result<RmuxPaneIo> {
    let pane = target.pane_selector().map(str::to_owned).with_context(|| {
        format!(
            "remote terminal session {} has no pane to attach",
            target.session_selector()
        )
    })?;
    remote.ensure_daemon()?;
    let session = target.session_selector().to_owned();
    let (output_tx, output_rx) = mpsc::channel();
    let (input_tx, input_rx) = tokio_mpsc::unbounded_channel();
    let (resize_tx, resize_rx) = tokio_mpsc::unbounded_channel();
    let (result_tx, result_rx) = mpsc::channel();

    spawn_output(
        remote,
        session.clone(),
        pane.clone(),
        max_scrollback,
        output_tx,
        result_tx.clone(),
    )?;
    spawn_input(
        remote,
        session.clone(),
        pane.clone(),
        input_rx,
        result_tx.clone(),
    )?;
    spawn_resize(remote, session, pane, resize_rx, result_tx);

    Ok(RmuxPaneIo {
        output_rx,
        input_tx,
        resize_tx,
        result_rx,
    })
}

#[cfg(feature = "app")]
fn remote_rmux_argv(
    remote: &SshRemote,
    request: &RemoteRmuxRequest,
) -> Result<(String, Vec<String>)> {
    let payload = encode_request(request)?;
    remote.proxy_command(
        crate::REMOTE_DAEMON_PROGRAM,
        &[REMOTE_RMUX_SUBCOMMAND.to_owned(), payload],
    )
}

#[cfg(feature = "app")]
fn encode_request(request: &RemoteRmuxRequest) -> Result<String> {
    let json = serde_json::to_vec(request).context("encode remote terminal request")?;
    if json.len() > MAX_REMOTE_RMUX_PAYLOAD {
        bail!("remote terminal request is too large")
    }
    Ok(URL_SAFE_NO_PAD.encode(json))
}

fn decode_request(payload: &str) -> Result<RemoteRmuxRequest> {
    if payload.len() > MAX_REMOTE_RMUX_PAYLOAD * 2 {
        bail!("remote terminal request is too large")
    }
    let json = URL_SAFE_NO_PAD
        .decode(payload)
        .context("decode remote terminal request")?;
    serde_json::from_slice(&json).context("parse remote terminal request")
}

pub fn run_remote_rmux_command(payload: &str) -> Result<i32> {
    match decode_request(payload)? {
        RemoteRmuxRequest::Snapshot => {
            println!("{}", serde_json::to_string(&rmux_snapshot()?)?);
        }
        RemoteRmuxRequest::Execute { command } => rmux_execute(command)?,
        RemoteRmuxRequest::PaneStream {
            session,
            pane,
            max_scrollback,
        } => stream_pane(session, pane, max_scrollback)?,
        RemoteRmuxRequest::PaneInput { session, pane } => input_pane(session, pane)?,
        RemoteRmuxRequest::Resize {
            session,
            pane,
            cols,
            rows,
        } => resize_rmux_pane(
            RmuxPaneTarget::new(session, Some(pane)),
            TerminalSizeSpec::new(cols, rows),
        )?,
    }
    Ok(0)
}

fn stream_pane(session: String, pane: String, max_scrollback: usize) -> Result<()> {
    let io = open_rmux_pane_io(RmuxPaneTarget::new(session, Some(pane)), max_scrollback)?;
    let mut stdout = BufWriter::new(std::io::stdout().lock());
    for event in io.output_rx {
        let frame = pane_frame(event);
        serde_json::to_writer(&mut stdout, &frame)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
        if matches!(frame, RemotePaneFrame::Error(_)) {
            break;
        }
    }
    Ok(())
}

fn pane_frame(event: RmuxPaneEvent) -> RemotePaneFrame {
    match event {
        RmuxPaneEvent::Restore {
            buffered_chunks,
            capture,
        } => RemotePaneFrame::Restore {
            capture: URL_SAFE_NO_PAD.encode(capture),
            buffered_chunks: encode_chunks(buffered_chunks),
        },
        RmuxPaneEvent::Chunks(chunks) => RemotePaneFrame::Chunks(encode_chunks(chunks)),
        RmuxPaneEvent::KeyboardProtocol(bytes) => {
            RemotePaneFrame::KeyboardProtocol(URL_SAFE_NO_PAD.encode(bytes))
        }
        RmuxPaneEvent::Error(error) => RemotePaneFrame::Error(error),
    }
}

fn encode_chunks(chunks: Vec<PaneOutputChunk>) -> Vec<String> {
    chunks
        .into_iter()
        .filter_map(|chunk| match chunk {
            PaneOutputChunk::Bytes { bytes, .. } => Some(URL_SAFE_NO_PAD.encode(bytes)),
            PaneOutputChunk::Lag(lag) if !lag.recent.bytes.is_empty() => {
                Some(URL_SAFE_NO_PAD.encode(lag.recent.bytes))
            }
            _ => None,
        })
        .collect()
}

#[cfg(feature = "app")]
fn decode_chunks(chunks: Vec<String>) -> Result<Vec<PaneOutputChunk>> {
    chunks
        .into_iter()
        .enumerate()
        .map(|(sequence, bytes)| {
            Ok(PaneOutputChunk::Bytes {
                sequence: sequence as u64,
                bytes: URL_SAFE_NO_PAD
                    .decode(bytes)
                    .context("decode remote terminal output")?,
            })
        })
        .collect()
}

fn input_pane(session: String, pane: String) -> Result<()> {
    let io = open_rmux_pane_io(RmuxPaneTarget::new(session, Some(pane)), 0)?;
    thread::spawn(move || for _ in io.output_rx {});
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let bytes = decode_input_line(&line?)?;
        io.input_tx
            .send(bytes)
            .map_err(|_| anyhow::anyhow!("remote terminal input stopped"))?;
        io.result_rx
            .recv()
            .context("remote terminal input worker stopped")?
            .map_err(anyhow::Error::msg)?;
    }
    Ok(())
}

fn decode_input_line(line: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(line)
        .context("decode remote terminal input")
}

#[cfg(feature = "app")]
fn spawn_output(
    remote: &SshRemote,
    session: String,
    pane: String,
    max_scrollback: usize,
    output_tx: mpsc::Sender<RmuxPaneEvent>,
    result_tx: mpsc::Sender<std::result::Result<(), String>>,
) -> Result<()> {
    let request = RemoteRmuxRequest::PaneStream {
        session,
        pane,
        max_scrollback,
    };
    let (program, args) = remote_rmux_argv(remote, &request)?;
    let mut child = Command::new(&program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("stream remote terminal pane")?;
    let stdout = child
        .stdout
        .take()
        .context("remote terminal output stream has no stdout")?;

    thread::spawn(move || {
        let _guard = ChildGuard(child);
        for line in BufReader::new(stdout).lines() {
            let result = line
                .map_err(anyhow::Error::from)
                .and_then(|line| serde_json::from_str::<RemotePaneFrame>(&line).map_err(Into::into))
                .and_then(decode_frame);
            match result {
                Ok(event) => {
                    if output_tx.send(event).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = result_tx.send(Err(format!("remote terminal output stopped: {error}")));
                    return;
                }
            }
        }
        let _ = result_tx.send(Err("remote terminal output ended".to_owned()));
    });
    Ok(())
}

#[cfg(feature = "app")]
fn decode_frame(frame: RemotePaneFrame) -> Result<RmuxPaneEvent> {
    Ok(match frame {
        RemotePaneFrame::Restore {
            capture,
            buffered_chunks,
        } => RmuxPaneEvent::Restore {
            capture: URL_SAFE_NO_PAD
                .decode(capture)
                .context("decode remote terminal restore")?,
            buffered_chunks: decode_chunks(buffered_chunks)?,
        },
        RemotePaneFrame::Chunks(chunks) => RmuxPaneEvent::Chunks(decode_chunks(chunks)?),
        RemotePaneFrame::KeyboardProtocol(bytes) => RmuxPaneEvent::KeyboardProtocol(
            URL_SAFE_NO_PAD
                .decode(bytes)
                .context("decode remote terminal protocol")?,
        ),
        RemotePaneFrame::Error(error) => RmuxPaneEvent::Error(error),
    })
}

#[cfg(feature = "app")]
fn spawn_input(
    remote: &SshRemote,
    session: String,
    pane: String,
    mut input_rx: tokio_mpsc::UnboundedReceiver<Vec<u8>>,
    result_tx: mpsc::Sender<std::result::Result<(), String>>,
) -> Result<()> {
    let request = RemoteRmuxRequest::PaneInput { session, pane };
    let (program, args) = remote_rmux_argv(remote, &request)?;
    let mut child = Command::new(&program)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("open remote terminal input")?;
    let stdin = child
        .stdin
        .take()
        .context("remote terminal input has no stdin")?;

    thread::spawn(move || {
        let _guard = ChildGuard(child);
        let mut writer = BufWriter::new(stdin);
        while let Some(bytes) = input_rx.blocking_recv() {
            if let Err(error) = write_input_line(&mut writer, &bytes) {
                let _ = result_tx.send(Err(format!("remote terminal input stopped: {error}")));
                return;
            }
        }
    });
    Ok(())
}

#[cfg(feature = "app")]
fn write_input_line(writer: &mut BufWriter<ChildStdin>, bytes: &[u8]) -> std::io::Result<()> {
    writer.write_all(URL_SAFE_NO_PAD.encode(bytes).as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[cfg(feature = "app")]
fn spawn_resize(
    remote: &SshRemote,
    session: String,
    pane: String,
    mut resize_rx: tokio_mpsc::UnboundedReceiver<TerminalSizeSpec>,
    result_tx: mpsc::Sender<std::result::Result<(), String>>,
) {
    let remote = remote.clone();
    thread::spawn(move || {
        while let Some(size) = resize_rx.blocking_recv() {
            let request = RemoteRmuxRequest::Resize {
                session: session.clone(),
                pane: pane.clone(),
                cols: size.cols,
                rows: size.rows,
            };
            let result = remote_rmux_argv(&remote, &request).and_then(|(program, args)| {
                let output = SystemCommandRunner.run(&program, &args)?;
                if output.success {
                    Ok(())
                } else {
                    bail!("{}", remote_daemon_failure(remote.host(), &output.stderr))
                }
            });
            let _ = result_tx.send(result.map_err(|error| error.to_string()));
        }
    });
}

#[cfg(feature = "app")]
struct ChildGuard(Child);

#[cfg(feature = "app")]
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "app")]
    use crate::controller::{BindingId, SpaceId};
    #[cfg(feature = "app")]
    use bootty_config::config::SshRemoteConfig;

    #[cfg(feature = "app")]
    fn remote() -> SshRemote {
        SshRemote::new(SshRemoteConfig::for_host("devbox"))
    }

    #[cfg(feature = "app")]
    #[test]
    fn remote_commands_use_boottys_embedded_protocol() {
        let (_program, argv) =
            remote_rmux_argv(&remote(), &RemoteRmuxRequest::Snapshot).expect("remote command");
        let (remote_program, args, _) =
            crate::ssh::decode_proxy_command_line(argv.last().expect("command"))
                .expect("decode command");

        assert_eq!(remote_program, crate::ssh::REMOTE_DAEMON_PROGRAM);
        assert_eq!(
            args.first().map(String::as_str),
            Some(REMOTE_RMUX_SUBCOMMAND)
        );
    }

    #[cfg(feature = "app")]
    #[test]
    fn remote_request_round_trips_hostile_command_arguments() {
        let request = RemoteRmuxRequest::Execute {
            command: MuxCommand::RenameSession {
                session_id: "space ; $HOME".to_owned(),
                name: "work & play\"".to_owned(),
            },
        };

        let decoded = decode_request(&encode_request(&request).unwrap()).unwrap();

        assert!(matches!(
            decoded,
            RemoteRmuxRequest::Execute {
                command: MuxCommand::RenameSession { session_id, name }
            } if session_id == "space ; $HOME" && name == "work & play\""
        ));
    }

    #[cfg(feature = "app")]
    #[test]
    fn pane_frames_preserve_binary_output() {
        let event = RmuxPaneEvent::Restore {
            capture: vec![0, 0xff, b'\n'],
            buffered_chunks: vec![PaneOutputChunk::Bytes {
                sequence: 7,
                bytes: vec![0x1b, b'[', b'm'],
            }],
        };

        let decoded = decode_frame(pane_frame(event)).unwrap();

        assert!(matches!(
            decoded,
            RmuxPaneEvent::Restore { capture, buffered_chunks }
                if capture == vec![0, 0xff, b'\n']
                    && matches!(buffered_chunks.as_slice(), [PaneOutputChunk::Bytes { bytes, .. }] if bytes == &vec![0x1b, b'[', b'm'])
        ));
    }

    #[cfg(feature = "app")]
    #[test]
    fn pane_frames_preserve_lag_recovery_bytes() {
        let bytes = vec![0x1b, b'[', b'm'];
        let event = RmuxPaneEvent::Chunks(vec![PaneOutputChunk::Lag(rmux_sdk::PaneLagNotice {
            expected_sequence: 1,
            resume_sequence: 2,
            missed_events: 1,
            newest_sequence: 2,
            recent: rmux_sdk::PaneRecentOutput {
                bytes: bytes.clone(),
                oldest_sequence: Some(2),
                newest_sequence: Some(2),
            },
        })]);

        let decoded = decode_frame(pane_frame(event)).unwrap();

        assert!(matches!(
            decoded,
            RmuxPaneEvent::Chunks(chunks)
                if matches!(chunks.as_slice(), [PaneOutputChunk::Bytes { bytes: recovered, .. }] if recovered == &bytes)
        ));
    }
    #[test]
    fn input_lines_preserve_exact_bytes() {
        let bytes = [0x00, 0x1b, 0xff];
        assert_eq!(
            decode_input_line(&URL_SAFE_NO_PAD.encode(bytes)).unwrap(),
            bytes
        );
    }

    #[cfg(feature = "app")]
    #[test]
    fn a_remote_rmux_binding_claims_rmux_operations() {
        let scope = MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(2));
        let backend = RemoteRmuxBackend::new(remote());

        assert_eq!(
            backend.capabilities(scope).operations().collect::<Vec<_>>(),
            rmux_capabilities(scope).operations().collect::<Vec<_>>()
        );
    }

    #[cfg(feature = "app")]
    #[test]
    fn a_session_without_a_pane_refuses_to_open() {
        let target = RmuxPaneTarget::new("project".to_owned(), None);

        let error = match open_remote_rmux_pane_io(&remote(), &target, 0) {
            Ok(_) => panic!("a session with no pane cannot be streamed"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("project"));
    }
}
