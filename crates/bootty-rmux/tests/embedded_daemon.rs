#![cfg(unix)]

use std::sync::{OnceLock, mpsc};
use std::thread;

use anyhow::{Context, Result};
use bootty_identity::ApplicationIdentity;
use bootty_mux::{
    command::{MuxCommand, MuxDirection, MuxSplitDirection},
    provider::MuxBackendRegistry,
    snapshot::{MuxSessionTag, new_session_identity},
    terminal::ActiveTerminal,
};
use bootty_mux_model::{MuxBackendKind, MuxBindingConfig};
use bootty_rmux::{RmuxBackend, endpoint_path_for};
use bootty_runtime::{frame_source::TerminalFrameSource, terminal_session::TerminalSessionConfig};
use bootty_surface::geometry::TerminalGeometry;
use tokio::runtime::Builder;

const SCENARIO_ENV: &str = "BOOTTY_RMUX_EMBEDDED_SCENARIO";
const SCENARIO_CHILD_TEST: &str = "embedded_rmux_scenario_child";
const ISOLATED_PATH: &str = "/usr/bin:/bin";
const POSIX_SHELL: &str = "/bin/sh";
/// What a prepared pane prints back. No scenario prints it for another reason.
const PANE_READY: &str = "BOOTTY_PANE_READY";
const PANE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const PANE_PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

fn start_embedded_rmux_daemon_for_tests() -> Result<()> {
    static STARTED: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    STARTED
        .get_or_init(|| {
            let socket = endpoint_path_for(ApplicationIdentity::Production)
                .map_err(|error| error.to_string())?;
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

/// Every scenario below drives bootty's public rmux behaviors through the SDK
/// only. Each gets its own `#[test]`, so a failure names the behavior that
/// broke and the test runner spends the wall clock of the slowest scenario
/// rather than the sum of all of them.
///
/// The scenario itself runs in a child process: the pane environment (`PATH`,
/// `SHELL`, `RMUX_TMPDIR`) is process-global, and a child is the only way to
/// set it without racing every other thread in the runner.
macro_rules! embedded_scenarios {
    ($($name:ident),+ $(,)?) => {
        $(
            #[test]
            fn $name() -> Result<()> {
                run_embedded_scenario(stringify!($name))
            }
        )+

        fn dispatch_embedded_scenario(name: &str) -> Result<()> {
            match name {
                $(stringify!($name) => scenario::$name(),)+
                unknown => anyhow::bail!("unknown embedded rmux scenario: {unknown}"),
            }
        }
    };
}

embedded_scenarios!(
    session_lifecycle,
    pane_navigation_and_zoom,
    terminal_requests,
    kitty_keyboard_protocol_reports_command_alt_key,
    terminal_queries_do_not_leak_into_the_shell,
    kitty_keyboard_protocol_pop_restores_legacy_ctrl_c,
    multi_pane_window_resize_keeps_pane_targets_live,
    closing_session_with_pending_resize_is_quiet,
    bounded_live_output,
    large_restore_progress,
);

/// The child-process entry point. A no-op in the runner's own process.
#[test]
fn embedded_rmux_scenario_child() -> Result<()> {
    let Some(scenario) = std::env::var_os(SCENARIO_ENV) else {
        return Ok(());
    };
    dispatch_embedded_scenario(&scenario.to_string_lossy())
}

mod scenario {
    use super::*;
    use pretty_assertions::assert_eq;

    pub fn session_lifecycle() -> Result<()> {
        let tag = MuxSessionTag {
            identity: Some(new_session_identity()),
            space: Some("space-under-test".to_owned()),
        };
        let (mut backend, registry, session_id, window_id, pane) =
            create_embedded_session(tag.clone())?;

        let snapshot = backend.snapshot()?;
        let session = snapshot
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("created rmux session");
        assert_eq!(session.tag, tag, "rmux reports the tag bootty stamped");

        let mut terminal = open_terminal(std::sync::Arc::clone(&registry), &pane, &window_id)?;
        prepare_pane(&mut terminal)?;
        terminal.write_input(b"printf 'BOOTTY_RMUX_FRAME\\n'\r")?;
        wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_FRAME")?;

        let mut second_terminal = open_terminal(registry, &pane, &window_id)?;
        wait_for_terminal_text(&mut second_terminal, "BOOTTY_RMUX_FRAME")?;

        drop(terminal);
        second_terminal.write_input(b"printf 'BOOTTY_RMUX_SECOND_READER\\n'\r")?;
        wait_for_terminal_text(&mut second_terminal, "BOOTTY_RMUX_SECOND_READER")?;

        let endpoint = endpoint_path_for(ApplicationIdentity::Production)?;
        let rmux_root = endpoint.parent().expect("embedded rmux endpoint parent");
        let spool_files = std::fs::read_dir(rmux_root)?
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .filter(|name| name.to_string_lossy().starts_with("bootty-rmux-output-"))
            .collect::<Vec<_>>();
        assert!(
            spool_files.is_empty(),
            "live rmux output created unbounded disk spools: {spool_files:?}"
        );

        backend.execute(MuxCommand::DitchSession {
            session_id: session_id.clone(),
        })?;
        let snapshot = backend.snapshot()?;
        assert!(
            !snapshot
                .sessions
                .iter()
                .any(|session| session.id == session_id)
        );
        Ok(())
    }

    pub fn pane_navigation_and_zoom() -> Result<()> {
        let (mut backend, _registry, session_id, window_id, _pane) =
            create_embedded_session(unscoped_tag())?;
        let initial = active_pane_id(&backend, &session_id)?;
        backend.execute(MuxCommand::SplitPane {
            session_id: session_id.clone(),
            pane_id: Some(initial.clone()),
            direction: MuxSplitDirection::Down,
        })?;

        let after_split = pane_ids(&backend, &session_id)?;
        assert_eq!(after_split.len(), 2, "split created a second rmux pane");
        let active_after_split = active_pane_id(&backend, &session_id)?;

        backend.execute(MuxCommand::SelectNextPane {
            session_id: session_id.clone(),
            window_id: Some(window_id.clone()),
        })?;
        let after_next = active_pane_id(&backend, &session_id)?;
        assert_ne!(
            after_next, active_after_split,
            "next pane changed the active pane"
        );

        backend.execute(MuxCommand::SelectPreviousPane {
            session_id: session_id.clone(),
            window_id: Some(window_id.clone()),
        })?;
        assert_eq!(active_pane_id(&backend, &session_id)?, active_after_split);

        backend.execute(MuxCommand::SelectPane {
            session_id: session_id.clone(),
            window_id: Some(window_id),
            direction: if active_after_split == after_split[0] {
                MuxDirection::Down
            } else {
                MuxDirection::Up
            },
        })?;
        assert_ne!(active_pane_id(&backend, &session_id)?, active_after_split);

        backend.execute(MuxCommand::TogglePaneZoom {
            session_id: session_id.clone(),
            pane_id: None,
        })?;
        backend.execute(MuxCommand::TogglePaneZoom {
            session_id: session_id.clone(),
            pane_id: None,
        })?;

        ditch_session(&mut backend, &session_id)
    }

    pub fn terminal_requests() -> Result<()> {
        let (mut backend, registry, session_id, window_id, pane) =
            create_embedded_session(unscoped_tag())?;
        let mut terminal = open_terminal(registry.clone(), &pane, &window_id)?;

        terminal.enter_copy_mode()?;
        assert!(terminal.copy_mode_active()?);
        let outcome = terminal.handle_copy_mode_action(
            bootty_terminal::terminal_engine::TerminalCopyModeAction::Cancel,
        )?;
        assert!(!outcome.active);
        assert_eq!(
            terminal.format_selection(
                bootty_terminal::terminal_engine::TerminalSelectionFormat::PlainText
            )?,
            None
        );
        assert!(!terminal.search_viewport(
            "",
            bootty_terminal::terminal_engine::TerminalSearchDirection::Current,
        )?);
        terminal.is_mouse_tracking()?;
        terminal.discard_pending_output()?;

        ditch_session(&mut backend, &session_id)
    }

    pub fn terminal_queries_do_not_leak_into_the_shell() -> Result<()> {
        // The round trip needs a program that can put the pane in raw mode and read
        // its own stdin. Skip rather than spending the whole `wait_for_terminal_text`
        // deadline on a shell that printed "command not found", which would take
        // every scenario chained after this one down with it.
        if !std::process::Command::new("python3")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
        {
            eprintln!("skipping terminal_queries_do_not_leak_into_the_shell: no python3 on PATH");
            return Ok(());
        }
        let (mut backend, registry, session_id, window_id, pane) =
            create_embedded_session(unscoped_tag())?;
        let mut terminal = open_terminal(registry.clone(), &pane, &window_id)?;
        prepare_pane(&mut terminal)?;
        // Reattach: delayed terminal replies used to cross this boundary and
        // land on the shell after the querying process exited.
        drop(terminal);
        let mut terminal = open_terminal(registry, &pane, &window_id)?;
        // Run from a file: inlining the script would put its backslash escapes
        // through the pane shell's quoting rules.
        let script_path =
            std::env::temp_dir().join(format!("bootty-terminal-query-{}.py", session_id));
        std::fs::write(
            &script_path,
            r#"import os, select, sys, termios, tty

fd = sys.stdin.fileno()
saved = termios.tcgetattr(fd)
tty.setraw(fd)
os.write(fd, b"\x1b[?u\x1b[c")
data = b""
# RMUX owns the pane PTY and answers DA1 synchronously. Consuming that answer
# models Crossterm's support probe, which then returns terminal ownership to the
# shell. Bootty must not inject a second, delayed response batch afterwards.
while b"c" not in data:
    if not select.select([fd], [], [], 2.0)[0]:
        break
    data += os.read(fd, 1)
os.write(fd, b"\x1b]11;?\x1b\\")
colour = b""
while b"\x1b\\" not in colour:
    if not select.select([fd], [], [], 2.0)[0]:
        break
    colour += os.read(fd, 1)
termios.tcsetattr(fd, termios.TCSADRAIN, saved)
if b"c" not in data:
    sys.exit("rmux did not answer DA1")
if b"rgb:" not in colour:
    sys.exit("Bootty did not answer OSC 11")
print("BOOTTY_RMUX_COLOR_QUERY_OK")
    "#,
        )?;
        terminal.write_input(
            format!(
                "stty echo; python3 {}; sleep 1; printf '%s%s\\n' 'BOOTTY_RMUX_QUERY' '_CLEAN'\r",
                script_path.display()
            )
            .as_bytes(),
        )?;
        wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_QUERY_CLEAN")?;
        terminal.drain_pty();
        let frame = terminal.extract_frame()?.text.iter().collect::<String>();
        anyhow::ensure!(
            frame.contains("BOOTTY_RMUX_COLOR_QUERY_OK")
                && !frame.contains("?0u")
                && !frame.contains("rgb:")
                && !frame.contains("62;22;52c"),
            "terminal replies leaked into the resumed shell: {frame:?}"
        );
        let _ = std::fs::remove_file(&script_path);

        ditch_session(&mut backend, &session_id)
    }

    pub fn kitty_keyboard_protocol_pop_restores_legacy_ctrl_c() -> Result<()> {
        let (mut backend, registry, session_id, window_id, pane) =
            create_embedded_session(unscoped_tag())?;
        let mut terminal = open_terminal(std::sync::Arc::clone(&registry), &pane, &window_id)?;
        prepare_pane(&mut terminal)?;
        // Push the kitty flags and pop them straight back off. Nothing queries
        // the state: the answer would arrive as pane input and land on the next
        // command line.
        terminal.write_input(
            b"printf '\\033[0m\\033[>1u\\033[<1u'; printf 'BOOTTY_RMUX_KEYBOARD_STATE\\n'\r",
        )?;
        wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_KEYBOARD_STATE")?;
        drop(terminal);

        let mut terminal = open_terminal(registry, &pane, &window_id)?;
        // Prove the reattached terminal is live before it has to carry a key.
        terminal.write_input(b"printf 'BOOTTY_RMUX_CTRL_C_PANE_LIVE\\n'\r")?;
        wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_CTRL_C_PANE_LIVE")?;
        terminal.write_input(
            b"/bin/sh -c 'trap \"printf BOOTTY_RMUX_CTRL_C_HANDLED\\\\n; exit 0\" INT; printf BOOTTY_RMUX_CTRL_C_READY\\n; while :; do read line; done'; stty echo; printf '\\101\\102\\103\\n'\r",
        )?;
        wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_CTRL_C_READY")?;
        terminal.encode_key(bootty_terminal::terminal_input_model::KeyInput {
            key: bootty_terminal::terminal_input_model::TerminalKey::C,
            mods: bootty_terminal::terminal_input_model::KeyMods {
                ctrl: true,
                ..Default::default()
            },
            repeat: false,
            utf8: Some("c"),
            unshifted: Some('c'),
        })?;
        wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_CTRL_C_HANDLED")?;
        wait_for_terminal_text(&mut terminal, "ABC")?;

        ditch_session(&mut backend, &session_id)
    }

    pub fn kitty_keyboard_protocol_reports_command_alt_key() -> Result<()> {
        if !std::process::Command::new("python3")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
        {
            eprintln!(
                "skipping kitty_keyboard_protocol_reports_command_alt_key: no python3 on PATH"
            );
            return Ok(());
        }
        let (mut backend, registry, session_id, window_id, pane) =
            create_embedded_session(unscoped_tag())?;
        let mut terminal = open_terminal(registry, &pane, &window_id)?;
        prepare_pane(&mut terminal)?;
        let script_path =
            std::env::temp_dir().join(format!("bootty-kitty-keyboard-{}.py", session_id));
        std::fs::write(
            &script_path,
            r#"import os, select, sys, termios, tty

fd = sys.stdin.fileno()
saved = termios.tcgetattr(fd)
tty.setraw(fd)
os.write(fd, b"\x1b[>7u\x1b[?u\x1b[cBOOTTY_RMUX_KITTY_READY\r\n")
data = b""
expected = b"\x1b[98;11u"
while expected not in data:
    if not select.select([fd], [], [], 10.0)[0]:
        break
    data += os.read(fd, 4096)
termios.tcsetattr(fd, termios.TCSADRAIN, saved)
if expected in data:
    print("BOOTTY_RMUX_COMMAND_ALT_KEY_OK")
"#,
        )?;
        terminal.write_input(format!("python3 {}\r", script_path.display()).as_bytes())?;
        wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_KITTY_READY")?;
        terminal.encode_key(bootty_terminal::terminal_input_model::KeyInput {
            key: bootty_terminal::terminal_input_model::TerminalKey::B,
            mods: bootty_terminal::terminal_input_model::KeyMods {
                alt: true,
                command: true,
                ..Default::default()
            },
            repeat: false,
            utf8: Some("b"),
            unshifted: Some('b'),
        })?;
        wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_COMMAND_ALT_KEY_OK")?;
        let _ = std::fs::remove_file(&script_path);

        ditch_session(&mut backend, &session_id)
    }

    pub fn multi_pane_window_resize_keeps_pane_targets_live() -> Result<()> {
        let (mut backend, registry, session_id, window_id, pane) =
            create_embedded_session(unscoped_tag())?;
        backend.execute(MuxCommand::SplitPane {
            session_id: session_id.clone(),
            pane_id: Some(pane.pane_id.clone().context("split pane id")?),
            direction: MuxSplitDirection::Down,
        })?;
        let snapshot = backend.snapshot()?;
        let (panes, focused) = snapshot
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| session.windows.first())
            .map(|window| {
                let focused = window
                    .panes
                    .iter()
                    .find(|pane| pane.pane_id == window.anchor.pane_id)
                    .or_else(|| window.panes.first())
                    .cloned();
                (window.panes.clone(), focused)
            })
            .context("split rmux window")?;
        let focused = focused.context("split rmux pane")?;
        let mut terminal = open_terminal_with_window(registry, &panes, &focused, &window_id)?;

        for _ in 0..8 {
            terminal.write_input(b"\x7f")?;
        }
        terminal.drain_pty();
        terminal.extract_frame()?;

        for cols in [96, 104, 112, 120] {
            terminal.resize_native_layout_window(cols, 30)?;
            terminal.drain_pty();
        }
        terminal.resize_native_layout_window(128, 30)?;

        ditch_session(&mut backend, &session_id)
    }

    pub fn closing_session_with_pending_resize_is_quiet() -> Result<()> {
        let (mut backend, registry, session_id, window_id, pane) =
            create_embedded_session(unscoped_tag())?;
        let mut terminal = open_terminal(registry, &pane, &window_id)?;

        terminal.resize_native_layout_window(96, 30)?;
        backend.execute(MuxCommand::DitchSession {
            session_id: session_id.clone(),
        })?;

        for cols in [100, 104, 108, 112] {
            terminal.resize_native_layout_window(cols, 30)?;
            terminal.drain_pty();
            terminal.extract_frame()?;
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Ok(())
    }

    pub fn bounded_live_output() -> Result<()> {
        let (mut backend, registry, session_id, window_id, pane) =
            create_embedded_session(unscoped_tag())?;
        let mut terminal = open_terminal(registry, &pane, &window_id)?;
        prepare_pane(&mut terminal)?;

        // 2MB is past the in-flight bound (`RMUX_OUTPUT_CHANNEL_CAPACITY` events
        // of `RMUX_OUTPUT_EVENT_MAX_BYTES`), so the producer has to be held back
        // rather than spooled to disk.
        terminal.write_input(
            b"printf 'BOOTTY_RMUX_BOUND_START\\n'; yes X | head -c 2000000; printf '\\nBOOTTY_RMUX_BOUND_END\\n'\r",
        )?;
        std::thread::sleep(std::time::Duration::from_millis(100));
        wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_BOUND_END")?;
        let spool_files = std::fs::read_dir(
            endpoint_path_for(ApplicationIdentity::Production)?
                .parent()
                .expect("embedded rmux endpoint parent"),
        )?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .filter(|name| name.to_string_lossy().starts_with("bootty-rmux-output-"))
        .collect::<Vec<_>>();
        assert!(
            spool_files.is_empty(),
            "bounded live output must not create disk spools: {spool_files:?}"
        );

        ditch_session(&mut backend, &session_id)
    }

    pub fn large_restore_progress() -> Result<()> {
        let (mut backend, registry, session_id, window_id, pane) =
            create_embedded_session(unscoped_tag())?;
        let mut producer = open_terminal(std::sync::Arc::clone(&registry), &pane, &window_id)?;
        prepare_pane(&mut producer)?;
        producer.write_input(b"yes RESTORE | head -c 2000000\r")?;
        wait_for_terminal_text(&mut producer, "RESTORE")?;

        // Attach mid-flood: the reader has a backlog to restore and still has to
        // take a resize and input.
        let mut reader = open_terminal(registry, &pane, &window_id)?;
        reader.resize_native_layout_window(100, 30)?;
        reader.write_input(b"printf 'BOOTTY_RMUX_RESTORE_INPUT_RESIZE\n'\r")?;
        wait_for_terminal_text(&mut reader, "BOOTTY_RMUX_RESTORE_INPUT_RESIZE")?;

        ditch_session(&mut backend, &session_id)
    }
}

/// Run one scenario in a child process with its own rmux daemon and a pane
/// environment that is the same on every machine.
fn run_embedded_scenario(scenario: &str) -> Result<()> {
    let directory = assert_fs::TempDir::new()?;
    let status = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", SCENARIO_CHILD_TEST])
        .env(SCENARIO_ENV, scenario)
        .env("RMUX_TMPDIR", directory.path())
        .env("BOOTTY_APPLICATION_IDENTITY", "bootty")
        .env("PATH", ISOLATED_PATH)
        // Panes inherit the daemon's environment. Pin the shell and its rc file
        // so a developer's login shell does not decide how long every pane takes
        // to answer: an interactive zsh with plugins costs seconds per pane.
        .env("SHELL", POSIX_SHELL)
        .env("ENV", "")
        .status()?;

    anyhow::ensure!(
        status.success(),
        "embedded rmux scenario {scenario} failed: {status}"
    );
    Ok(())
}

fn unscoped_tag() -> MuxSessionTag {
    MuxSessionTag {
        identity: Some(new_session_identity()),
        space: None,
    }
}

fn create_embedded_session(
    tag: MuxSessionTag,
) -> Result<(
    RmuxBackend,
    std::sync::Arc<MuxBackendRegistry>,
    String,
    String,
    bootty_mux::snapshot::MuxPaneAnchor,
)> {
    start_embedded_rmux_daemon_for_tests()?;
    bootty_rmux::link();
    let registry = std::sync::Arc::new(MuxBackendRegistry::collect([MuxBackendKind::Rmux])?);
    let session_id = format!("bootty-mux-test-{}", std::process::id());
    let mut backend = RmuxBackend::new();
    backend.execute(MuxCommand::CreateProjectSession {
        session_id: session_id.clone(),
        cwd: std::env::temp_dir().to_string_lossy().into_owned(),
        tag,
    })?;

    let snapshot = backend.snapshot()?;
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("created rmux session");
    let window = session.windows.first().expect("created rmux window");
    let pane = window.panes.first().expect("created rmux pane").clone();
    Ok((backend, registry, session_id, window.id.clone(), pane))
}

fn open_terminal(
    registry: std::sync::Arc<MuxBackendRegistry>,
    pane: &bootty_mux::snapshot::MuxPaneAnchor,
    window_id: &str,
) -> Result<ActiveTerminal> {
    open_terminal_with_window(registry, std::slice::from_ref(pane), pane, window_id)
}

fn open_terminal_with_window(
    registry: std::sync::Arc<MuxBackendRegistry>,
    panes: &[bootty_mux::snapshot::MuxPaneAnchor],
    focused: &bootty_mux::snapshot::MuxPaneAnchor,
    window_id: &str,
) -> Result<ActiveTerminal> {
    let mut terminal = ActiveTerminal::new(
        TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        },
        registry,
        &MuxBindingConfig {
            backend: MuxBackendKind::Rmux,
            ..MuxBindingConfig::default()
        },
        TerminalSessionConfig::default(),
        std::sync::Arc::new(|| {}),
    );
    terminal.sync_native_window(
        panes,
        Some(focused),
        Some(window_id),
        MuxBackendKind::Rmux,
        false,
    )?;
    Ok(terminal)
}

fn ditch_session(backend: &mut RmuxBackend, session_id: &str) -> Result<()> {
    backend.execute(MuxCommand::DitchSession {
        session_id: session_id.to_owned(),
    })?;
    let snapshot = backend.snapshot()?;
    assert!(
        !snapshot
            .sessions
            .iter()
            .any(|session| session.id == session_id)
    );
    Ok(())
}

fn active_pane_id(backend: &RmuxBackend, session_id: &str) -> Result<String> {
    backend
        .snapshot()?
        .sessions
        .into_iter()
        .find(|session| session.id == session_id)
        .context("rmux test session was not found")?
        .windows
        .into_iter()
        .find(|window| window.active)
        .context("rmux test active window was not found")?
        .anchor
        .pane_id
        .context("rmux test active pane was not found")
}

fn pane_ids(backend: &RmuxBackend, session_id: &str) -> Result<Vec<String>> {
    Ok(backend
        .snapshot()?
        .sessions
        .into_iter()
        .find(|session| session.id == session_id)
        .context("rmux test session was not found")?
        .windows
        .into_iter()
        .find(|window| window.active)
        .context("rmux test active window was not found")?
        .panes
        .into_iter()
        .filter_map(|pane| pane.pane_id)
        .collect())
}

/// Poll the pane's frames until `matches` holds. `Some(text)` is the last frame
/// seen before the deadline passed, so a failure reports what the pane was
/// actually showing.
fn poll_frames_until(
    terminal: &mut ActiveTerminal,
    deadline: std::time::Instant,
    matches: impl Fn(&str) -> bool,
) -> Result<Option<String>> {
    loop {
        terminal.drain_pty();
        let frame = terminal.extract_frame()?;
        let text = frame.text.iter().collect::<String>();
        if matches(&text) {
            return Ok(None);
        }
        if std::time::Instant::now() >= deadline {
            return Ok(Some(text));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn wait_for_terminal_text(terminal: &mut ActiveTerminal, expected: &str) -> Result<()> {
    let deadline = std::time::Instant::now() + PANE_TIMEOUT;
    match poll_frames_until(terminal, deadline, |text| text.contains(expected))? {
        None => Ok(()),
        Some(text) => anyhow::bail!(
            "rmux terminal did not publish {expected:?}, last frame: {:?}",
            text.trim_end()
        ),
    }
}

/// Put the pane in the state every scenario expects: a shell that has claimed
/// the pty, with echo off.
///
/// The probe is retried instead of waited on, because a pane drops input written
/// before its shell claims the pty and prints nothing to announce that moment.
/// Repeating the probe is harmless -- both halves are idempotent -- and it is
/// the only part of a scenario that may run more than once.
///
/// Echo off matters as much as readiness: with it on, the pane's frame holds the
/// command line that was typed, and a wait for a marker is satisfied by the
/// command asking for it rather than by the pane printing it.
fn prepare_pane(terminal: &mut ActiveTerminal) -> Result<()> {
    let deadline = std::time::Instant::now() + PANE_TIMEOUT;
    loop {
        // Split so the echo of this line cannot answer for the pane.
        terminal.write_input(b"stty -echo; printf '%s%s\\n' 'BOOTTY_PANE' '_READY'\r")?;
        let attempt = (std::time::Instant::now() + PANE_PROBE_INTERVAL).min(deadline);
        if poll_frames_until(terminal, attempt, |text| text.contains(PANE_READY))?.is_none() {
            return Ok(());
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "pane shell never answered a readiness probe"
        );
    }
}
