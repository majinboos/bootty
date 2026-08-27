#![cfg(unix)]

use std::sync::{Mutex, OnceLock, mpsc};
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
use pretty_assertions::assert_eq;
use tokio::runtime::Builder;

const HELPER_ENV: &str = "BOOTTY_RMUX_EMBEDDED_HELPER";
const ISOLATED_PATH: &str = "/usr/bin:/bin";

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

#[test]
fn embedded_rmux_public_behaviors_use_only_the_sdk() -> Result<()> {
    run_embedded_helper("embedded_rmux_public_behaviors_use_only_the_sdk_helper")
}

#[test]
fn embedded_rmux_public_behaviors_use_only_the_sdk_helper() -> Result<()> {
    if std::env::var_os(HELPER_ENV).is_none() {
        return Ok(());
    }
    session_lifecycle()?;
    pane_navigation_and_zoom()?;
    terminal_requests()?;
    color_query_round_trip()?;
    kitty_keyboard_protocol_pop_restores_legacy_ctrl_c()?;
    multi_pane_window_resize_keeps_pane_targets_live()?;
    closing_session_with_pending_resize_is_quiet()?;
    bounded_live_output()?;
    large_restore_progress()
}

fn session_lifecycle() -> Result<()> {
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

fn pane_navigation_and_zoom() -> Result<()> {
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

fn terminal_requests() -> Result<()> {
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

fn color_query_round_trip() -> Result<()> {
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
        eprintln!("skipping color_query_round_trip: no python3 on PATH");
        return Ok(());
    }
    let (mut backend, registry, session_id, window_id, pane) =
        create_embedded_session(unscoped_tag())?;
    let mut terminal = open_terminal(registry.clone(), &pane, &window_id)?;
    // Split so the echoed command line does not itself satisfy the wait: the
    // frame text includes what was typed, not just what the pane printed.
    terminal
        .write_input(b"printf '\\033[?u'; printf '%s%s\\n' 'BOOTTY_RMUX_REBASE' '_PREPARED'\r")?;
    wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_REBASE_PREPARED")?;
    drop(terminal);
    let mut terminal = open_terminal(registry, &pane, &window_id)?;
    // Run from a file: the pane's shell is whatever $SHELL is, and quoting rules
    // differ enough between them (fish collapses backslashes inside single
    // quotes) to corrupt an inlined script.
    let script_path = std::env::temp_dir().join(format!("bootty-color-query-{}.py", session_id));
    std::fs::write(
        &script_path,
        r#"import os, sys, select, termios, tty

fd = sys.stdin.fileno()
saved = termios.tcgetattr(fd)
tty.setraw(fd)
os.write(fd, b"\x1b]11;?\x1b\\\x1b[c")
data = b""
# Read until the colour answer, not until the first CSI response: rmux answers
# DA1 itself as a VT100, which arrives before anything Bootty writes back.
while b"rgb:" not in data:
    if not select.select([fd], [], [], 5.0)[0]:
        break
    data += os.read(fd, 4096)
termios.tcsetattr(fd, termios.TCSADRAIN, saved)
if b"rgb:" in data:
    print("BOOTTY_RMUX_COLOR" + "_QUERY_OK")
"#,
    )?;
    // Kill the line first: answering the query above put `\x1b[?0u` on the
    // pane's input, and the shell's line editor leaves the printable tail of it
    // sitting on the command line.
    terminal.write_input(b"\x15")?;
    terminal.write_input(format!("python3 {}\r", script_path.display()).as_bytes())?;
    wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_COLOR_QUERY_OK")?;
    let _ = std::fs::remove_file(&script_path);

    ditch_session(&mut backend, &session_id)
}

fn kitty_keyboard_protocol_pop_restores_legacy_ctrl_c() -> Result<()> {
    let (mut backend, registry, session_id, window_id, pane) =
        create_embedded_session(unscoped_tag())?;
    let mut terminal = open_terminal(std::sync::Arc::clone(&registry), &pane, &window_id)?;
    terminal.write_input(
        b"printf '\\033[0m\\033[>1u\\033[?u\\033[<1u'; printf '%s%s\n' 'BOOTTY_RMUX_KEYBOARD' '_STATE'\r",
    )?;
    wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_KEYBOARD_STATE")?;
    drop(terminal);

    let mut terminal = open_terminal(registry, &pane, &window_id)?;
    terminal
        .write_input(b"stty -echo; printf '%s%s\\n' 'BOOTTY_RMUX_CTRL_C' '_ECHO_DISABLED'\r")?;
    wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_CTRL_C_ECHO_DISABLED")?;
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

fn multi_pane_window_resize_keeps_pane_targets_live() -> Result<()> {
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

fn closing_session_with_pending_resize_is_quiet() -> Result<()> {
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

fn bounded_live_output() -> Result<()> {
    let (mut backend, registry, session_id, window_id, pane) =
        create_embedded_session(unscoped_tag())?;
    let mut terminal = open_terminal(registry, &pane, &window_id)?;

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

fn large_restore_progress() -> Result<()> {
    let (mut backend, registry, session_id, window_id, pane) =
        create_embedded_session(unscoped_tag())?;
    let mut producer = open_terminal(std::sync::Arc::clone(&registry), &pane, &window_id)?;
    producer.write_input(b"yes RESTORE | head -c 2000000\r")?;
    wait_for_terminal_text(&mut producer, "RESTORE")?;

    let mut reader = open_terminal(registry, &pane, &window_id)?;
    reader.resize_native_layout_window(100, 30)?;
    reader.write_input(b"printf 'BOOTTY_RMUX_RESTORE_INPUT_RESIZE\n'\r")?;
    wait_for_terminal_text(&mut reader, "BOOTTY_RMUX_RESTORE_INPUT_RESIZE")?;

    ditch_session(&mut backend, &session_id)
}

fn run_embedded_helper(name: &str) -> Result<()> {
    static HELPER_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = (std::env::var_os(HELPER_ENV).is_none()).then(|| {
        HELPER_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("embedded RMUX helper lock")
    });
    let directory = assert_fs::TempDir::new()?;
    let status = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", name])
        .env(HELPER_ENV, "1")
        .env("RMUX_TMPDIR", directory.path())
        .env("BOOTTY_APPLICATION_IDENTITY", "bootty")
        .env("PATH", ISOLATED_PATH)
        .status()?;

    anyhow::ensure!(status.success(), "embedded rmux helper failed: {status}");
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

fn wait_for_terminal_text(terminal: &mut ActiveTerminal, expected: &str) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        terminal.drain_pty();
        let frame = terminal.extract_frame()?;
        if frame.text.iter().collect::<String>().contains(expected) {
            return Ok(());
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "rmux terminal did not publish {expected:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
