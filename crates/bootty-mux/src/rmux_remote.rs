//! Drives an rmux pane that lives on another host, through that host's own rmux client.
//!
//! The local rmux path talks to a daemon over a unix socket and tails its pane output through files
//! the daemon writes beside that socket — both of which are on the daemon's machine and neither of
//! which crosses an SSH connection. rmux's command line reaches the same daemon from a shell there,
//! so a remote pane is driven by three commands run over SSH instead: `stream-pane` for output,
//! `send-keys` for input, and `resize-window` for geometry.
//!
//! What this fills is [`RmuxPaneIo`], the same three channels the local path fills, so everything
//! above it — the pane worker, the terminal, the renderer — cannot tell the difference. That is
//! also where a transport built on bootty itself would attach.

use std::{
    io::{BufWriter, Read, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
};

use anyhow::{Context, Result};
use rmux_sdk::TerminalSizeSpec;

use crate::{
    backend::MuxBackend,
    capability::BindingCapabilityDescriptor,
    command::MuxCommand,
    controller::MuxScope,
    process::SystemCommandRunner,
    rmux::rmux_capabilities,
    rmux_bridge::{RmuxPaneEvent, RmuxPaneIo, RmuxPaneTarget},
    snapshot::MuxSnapshot,
    ssh::{SshCommandRunner, SshRemote},
    tmux::TmuxBackend,
};
use rmux_sdk::PaneOutputChunk;
use tokio::sync::mpsc as tokio_mpsc;

/// Bootty's daemon on the remote host, named the same way it is here. rmux resolves a `-L` label to
/// a socket path with the remote's own user and temporary directory, so the path never has to be
/// guessed from this side.
pub(crate) fn bootty_socket_label() -> String {
    crate::bootty_rmux_socket_name(rmux_proto::RMUX_WIRE_VERSION)
}

/// Reads and drives a remote bootty rmux daemon through that host's rmux command line.
///
/// rmux answers the tmux command surface, and every session, window and pane field bootty asks for
/// is already a tmux-style format string, so the tmux adapter maps the commands unchanged. Only the
/// capability list differs: this is an rmux binding and claims what rmux does, not what tmux adds.
pub struct RemoteRmuxBackend {
    inner: TmuxBackend<SshCommandRunner<SystemCommandRunner>>,
}

impl RemoteRmuxBackend {
    pub fn new(remote: SshRemote) -> Self {
        Self {
            inner: TmuxBackend::with_runner(
                "rmux",
                SshCommandRunner::with_leading_args(
                    remote,
                    SystemCommandRunner,
                    vec!["-L".to_owned(), bootty_socket_label()],
                ),
            ),
        }
    }
}

impl MuxBackend for RemoteRmuxBackend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        self.inner.snapshot()
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        self.inner.execute(command)
    }

    fn capabilities(&self, scope: MuxScope) -> BindingCapabilityDescriptor {
        rmux_capabilities(scope)
    }
}

/// Open a remote pane's streams. The pane's history arrives once as a restore, then output follows
/// live for as long as the pane is attached.
pub(crate) fn open_remote_rmux_pane_io(
    remote: &SshRemote,
    target: &RmuxPaneTarget,
    max_scrollback_bytes: usize,
) -> Result<RmuxPaneIo> {
    let pane = pane_selector(target)?;
    let (output_tx, output_rx) = mpsc::channel();
    let (input_tx, input_rx) = tokio_mpsc::unbounded_channel();
    let (resize_tx, resize_rx) = tokio_mpsc::unbounded_channel();
    let (result_tx, result_rx) = mpsc::channel();

    spawn_restore(remote, &pane, max_scrollback_bytes, output_tx.clone());
    spawn_output(remote, &pane, output_tx, result_tx.clone())?;
    spawn_input(remote, &pane, input_rx, result_tx.clone())?;
    spawn_resize(
        remote,
        target.session_selector().to_owned(),
        resize_rx,
        result_tx,
    );

    Ok(RmuxPaneIo {
        output_rx,
        input_tx,
        resize_tx,
        result_rx,
    })
}

/// The pane to address, which rmux names the way tmux does.
fn pane_selector(target: &RmuxPaneTarget) -> Result<String> {
    target.pane_selector().map(str::to_owned).with_context(|| {
        format!(
            "rmux session {} has no pane to attach",
            target.session_selector()
        )
    })
}

fn rmux_argv(remote: &SshRemote, args: &[&str]) -> (String, Vec<String>) {
    let mut rmux = vec!["-L".to_owned(), bootty_socket_label()];
    rmux.extend(args.iter().map(|arg| (*arg).to_owned()));
    remote.command("rmux", &rmux)
}

/// The pane's retained output, sent as the restore the pane worker expects before live output.
/// A pane with nothing retained, or a host that cannot answer, still starts: the pane simply begins
/// empty rather than refusing to attach.
fn spawn_restore(
    remote: &SshRemote,
    pane: &str,
    max_scrollback_bytes: usize,
    output_tx: mpsc::Sender<RmuxPaneEvent>,
) {
    let (program, args) = rmux_argv(
        remote,
        &[
            "collect-pane-output",
            "-t",
            pane,
            "--max-bytes",
            &max_scrollback_bytes.to_string(),
        ],
    );
    thread::spawn(move || {
        let capture = Command::new(&program)
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| output.stdout)
            .unwrap_or_default();
        let _ = output_tx.send(RmuxPaneEvent::Restore {
            buffered_chunks: Vec::new(),
            capture,
        });
    });
}

/// Live pane output. `stream-pane --raw` writes the pane's bytes to its standard output, so the SSH
/// connection carries them unchanged and the pane worker sees the same chunks the local path
/// produces.
fn spawn_output(
    remote: &SshRemote,
    pane: &str,
    output_tx: mpsc::Sender<RmuxPaneEvent>,
    result_tx: mpsc::Sender<std::result::Result<(), String>>,
) -> Result<()> {
    let (program, args) = rmux_argv(remote, &["stream-pane", "--raw", "-t", pane]);
    let mut child = Command::new(&program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("stream remote rmux pane {pane}"))?;
    let mut stdout = child
        .stdout
        .take()
        .context("remote rmux output stream has no stdout")?;

    thread::spawn(move || {
        let _guard = ChildGuard(child);
        let mut buffer = vec![0u8; 64 * 1024];
        let mut sequence = 0u64;
        loop {
            match stdout.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    sequence = sequence.saturating_add(1);
                    let chunk = PaneOutputChunk::Bytes {
                        sequence,
                        bytes: buffer[..read].to_vec(),
                    };
                    if output_tx.send(RmuxPaneEvent::Chunks(vec![chunk])).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ =
                        result_tx.send(Err(format!("remote rmux pane output stopped: {error}")));
                    return;
                }
            }
        }
        // The stream ending means the pane is gone or the connection dropped. Either way the pane
        // has no more output, and the snapshot decides which of the two it was.
        let _ = result_tx.send(Err("remote rmux pane output ended".to_owned()));
    });
    Ok(())
}

/// Keystrokes, as lines of hex on one open connection. A keypress must not cost an SSH handshake,
/// so the remote runs a loop that reads a line and hands those bytes to the pane, and every write
/// afterwards is a line on a pipe that is already open.
const REMOTE_INPUT_SCRIPT: &str = r#"label=$1
pane=$2
while IFS= read -r hex; do
  rmux -L "$label" send-keys -t "$pane" -H $hex || exit 1
done
"#;

fn spawn_input(
    remote: &SshRemote,
    pane: &str,
    mut input_rx: tokio_mpsc::UnboundedReceiver<Vec<u8>>,
    result_tx: mpsc::Sender<std::result::Result<(), String>>,
) -> Result<()> {
    let (program, args) = remote.command(
        "sh",
        &[
            "-c".to_owned(),
            REMOTE_INPUT_SCRIPT.to_owned(),
            "bootty-rmux-input".to_owned(),
            bootty_socket_label(),
            pane.to_owned(),
        ],
    );
    let mut child = Command::new(&program)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("open remote rmux input for pane {pane}"))?;
    let stdin = child
        .stdin
        .take()
        .context("remote rmux input has no stdin")?;

    thread::spawn(move || {
        let _guard = ChildGuard(child);
        let mut writer = BufWriter::new(stdin);
        while let Some(bytes) = input_rx.blocking_recv() {
            if let Err(error) = write_hex_line(&mut writer, &bytes) {
                let _ = result_tx.send(Err(format!("remote rmux pane input stopped: {error}")));
                return;
            }
        }
    });
    Ok(())
}

fn write_hex_line(writer: &mut BufWriter<ChildStdin>, bytes: &[u8]) -> std::io::Result<()> {
    writer.write_all(hex_line(bytes).as_bytes())?;
    writer.flush()
}

/// One `send-keys -H` argument list per write: hex byte values, separated by spaces, on one line.
fn hex_line(bytes: &[u8]) -> String {
    let mut line = String::with_capacity(bytes.len() * 3 + 1);
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            line.push(' ');
        }
        line.push_str(&format!("{byte:02x}"));
    }
    line.push('\n');
    line
}

/// Geometry, which changes when a window is resized rather than as fast as it is typed into, so it
/// can afford its own invocation. The window is what carries the size: a pane cannot be made larger
/// than the window holding it, and a window only takes a size of its own once it stops following
/// whichever client is attached.
fn spawn_resize(
    remote: &SshRemote,
    session: String,
    mut resize_rx: tokio_mpsc::UnboundedReceiver<TerminalSizeSpec>,
    result_tx: mpsc::Sender<std::result::Result<(), String>>,
) {
    let remote = remote.clone();
    thread::spawn(move || {
        while let Some(size) = resize_rx.blocking_recv() {
            let cols = size.cols.to_string();
            let rows = size.rows.to_string();
            let manual = rmux_argv(
                &remote,
                &["set-option", "-t", &session, "window-size", "manual"],
            );
            let resize = rmux_argv(
                &remote,
                &["resize-window", "-t", &session, "-x", &cols, "-y", &rows],
            );
            for (program, args) in [manual, resize] {
                let outcome = Command::new(&program)
                    .args(&args)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                if let Err(error) = outcome {
                    let _ = result_tx.send(Err(format!("remote rmux resize failed: {error}")));
                    break;
                }
            }
        }
    });
}

/// Ends the SSH child with the thread that reads it, so a closed pane does not leave a connection
/// open on either machine.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        controller::{BindingId, SpaceId},
        process::{CommandOutput, CommandRunner},
    };
    use bootty_config::config::SshRemoteConfig;
    use std::cell::RefCell;

    type RecordedCalls = std::rc::Rc<RefCell<Vec<(String, Vec<String>)>>>;

    #[derive(Default)]
    struct RecordingRunner {
        calls: RecordedCalls,
    }

    impl CommandRunner for RecordingRunner {
        fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
            self.calls
                .borrow_mut()
                .push((program.to_owned(), args.to_vec()));
            Ok(CommandOutput {
                success: true,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    fn remote() -> SshRemote {
        SshRemote::new(SshRemoteConfig::for_host("devbox"))
    }

    /// `send-keys -H` reads byte values, so every byte has to arrive as two hex digits — a value
    /// written short would be read as a different key, and one line has to hold exactly one write.
    #[test]
    fn input_bytes_become_one_hex_line_per_write() {
        assert_eq!(hex_line(b"hi\r"), "68 69 0d\n");
        assert_eq!(hex_line(&[0x00, 0x1b, 0xff]), "00 1b ff\n");
        assert_eq!(hex_line(b""), "\n");
    }

    /// Every remote command has to name bootty's own daemon rather than whichever one the host
    /// happens to default to, and the label is what makes the remote resolve that socket itself.
    #[test]
    fn remote_commands_address_bootty_daemon_by_label() {
        let (program, argv) = rmux_argv(&remote(), &["stream-pane", "--raw", "-t", "%3"]);

        assert_eq!(program, "ssh");
        assert_eq!(
            argv.last().map(String::as_str),
            Some(
                format!(
                    "'rmux' '-L' '{}' 'stream-pane' '--raw' '-t' '%3'",
                    bootty_socket_label()
                )
                .as_str()
            )
        );
        assert!(bootty_socket_label().starts_with("bootty-wire"));
    }

    /// The control path answers through the tmux adapter, which would otherwise advertise what tmux
    /// can do. A binding that claims an operation its multiplexer lacks offers the user a command
    /// that fails when they run it.
    #[test]
    fn a_remote_rmux_binding_claims_rmux_operations_not_tmux_ones() {
        let scope = MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(2));
        let backend = RemoteRmuxBackend::new(remote());

        let operations = backend.capabilities(scope).operations().collect::<Vec<_>>();

        assert_eq!(
            operations,
            rmux_capabilities(scope).operations().collect::<Vec<_>>()
        );
        assert!(!operations.contains(&crate::capability::BindingOperation::TogglePaneZoom));
    }

    /// Every command the control path sends has to name bootty's daemon, or it reaches whichever
    /// server the remote defaults to and shows sessions that are not the ones being attached.
    #[test]
    fn control_commands_carry_the_daemon_label_before_their_own_arguments() {
        let recorder = RecordingRunner::default();
        let calls = recorder.calls.clone();
        let runner = SshCommandRunner::with_leading_args(
            remote(),
            recorder,
            vec!["-L".to_owned(), bootty_socket_label()],
        );

        runner
            .run("rmux", &["list-sessions".to_owned(), "-F".to_owned()])
            .unwrap();

        let calls = calls.borrow();
        assert_eq!(
            calls[0].1.last().map(String::as_str),
            Some(
                format!(
                    "'rmux' '-L' '{}' 'list-sessions' '-F'",
                    bootty_socket_label()
                )
                .as_str()
            )
        );
    }

    /// A pane bootty never learned the id of cannot be streamed, and failing here says so instead
    /// of opening streams against the session and rendering another pane's output.
    #[test]
    fn a_session_without_a_pane_refuses_to_open() {
        let target = RmuxPaneTarget::new("project".to_owned(), None);

        let error = pane_selector(&target).expect_err("a session with no pane cannot be streamed");

        assert!(error.to_string().contains("project"));
    }
}
