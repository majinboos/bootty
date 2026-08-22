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
use tokio::runtime::Builder;

const HELPER_ENV: &str = "BOOTTY_RMUX_EMBEDDED_CONTRACT_HELPER";
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
fn embedded_rmux_owns_session_lifecycle_without_an_external_executable() -> Result<()> {
    run_embedded_helper(
        "embedded_rmux_owns_session_lifecycle_without_an_external_executable_helper",
    )
}

#[test]
fn embedded_rmux_owns_session_lifecycle_without_an_external_executable_helper() -> Result<()> {
    if std::env::var_os(HELPER_ENV).is_none() {
        return Ok(());
    }

    start_embedded_rmux_daemon_for_tests()?;
    bootty_rmux::link();
    let registry = std::sync::Arc::new(MuxBackendRegistry::collect([MuxBackendKind::Rmux])?);
    let session_id = format!("bootty-mux-contract-{}", std::process::id());
    let mut backend = RmuxBackend::new();
    let tag = MuxSessionTag {
        identity: Some(new_session_identity()),
        space: Some("space-under-test".to_owned()),
    };
    backend.execute(MuxCommand::CreateProjectSession {
        session_id: session_id.clone(),
        cwd: std::env::temp_dir().to_string_lossy().into_owned(),
        tag: tag.clone(),
    })?;

    let snapshot = backend.snapshot()?;
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("created rmux session");
    // The whole membership design rests on rmux resolving `@` user options inside a list format,
    // the same way tmux does. If that ever stops holding, every Space claim silently empties.
    assert_eq!(session.tag, tag, "rmux reports the tag bootty stamped");
    let window = session.windows.first().expect("created rmux window");
    let pane = window.panes.first().expect("created rmux pane").clone();

    let mut terminal = open_terminal(std::sync::Arc::clone(&registry), &pane, &window.id)?;
    terminal.write_input(b"printf 'BOOTTY_RMUX_FRAME\\n'\r")?;
    wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_FRAME")?;

    // The second reader must continue from its restore into a later live event.
    let mut second_terminal = open_terminal(registry, &pane, &window.id)?;
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

#[test]
fn embedded_rmux_fresh_reader_restore_does_not_block_later_live_output() -> Result<()> {
    run_embedded_helper(
        "embedded_rmux_fresh_reader_restore_does_not_block_later_live_output_helper",
    )
}

#[test]
fn embedded_rmux_supports_pane_navigation_and_zoom() -> Result<()> {
    run_embedded_helper("embedded_rmux_supports_pane_navigation_and_zoom_helper")
}

#[test]
fn embedded_rmux_supports_pane_navigation_and_zoom_helper() -> Result<()> {
    if std::env::var_os(HELPER_ENV).is_none() {
        return Ok(());
    }

    let (mut backend, _registry, session_id, window_id, _pane) = create_embedded_session()?;
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

#[test]
fn embedded_rmux_fresh_reader_restore_does_not_block_later_live_output_helper() -> Result<()> {
    if std::env::var_os(HELPER_ENV).is_none() {
        return Ok(());
    }

    let (mut backend, registry, session_id, window_id, pane) = create_embedded_session()?;
    let mut terminal = open_terminal(registry, &pane, &window_id)?;

    // A fresh reader must leave restore mode and publish later live output.
    terminal.write_input(b"printf 'BOOTTY_RMUX_EMPTY_RESTORE_LIVE\\n'\r")?;
    wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_EMPTY_RESTORE_LIVE")?;

    ditch_session(&mut backend, &session_id)
}

#[test]
fn embedded_rmux_terminal_requests_keep_public_results() -> Result<()> {
    run_embedded_helper("embedded_rmux_terminal_requests_keep_public_results_helper")
}

#[test]
fn embedded_rmux_terminal_requests_keep_public_results_helper() -> Result<()> {
    if std::env::var_os(HELPER_ENV).is_none() {
        return Ok(());
    }

    let (mut backend, registry, session_id, window_id, pane) = create_embedded_session()?;
    let mut terminal = open_terminal(registry, &pane, &window_id)?;

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

#[test]
fn embedded_rmux_backpressures_live_output_without_reordering_tail() -> Result<()> {
    run_embedded_helper("embedded_rmux_backpressures_live_output_without_reordering_tail_helper")
}

#[test]
fn embedded_rmux_backpressures_live_output_without_reordering_tail_helper() -> Result<()> {
    if std::env::var_os(HELPER_ENV).is_none() {
        return Ok(());
    }

    let (mut backend, registry, session_id, window_id, pane) = create_embedded_session()?;
    let mut terminal = open_terminal(registry, &pane, &window_id)?;

    terminal.write_input(
        b"printf 'BOOTTY_RMUX_BOUND_START\\n'; yes X | head -c 2000000; printf '\\nBOOTTY_RMUX_BOUND_END\\n'\r",
    )?;
    // Let the producer fill the bounded handoff before the consumer starts draining it.
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

#[test]
fn embedded_rmux_large_restore_keeps_input_and_resize_progress() -> Result<()> {
    run_embedded_helper("embedded_rmux_large_restore_keeps_input_and_resize_progress_helper")
}

#[test]
fn embedded_rmux_large_restore_keeps_input_and_resize_progress_helper() -> Result<()> {
    if std::env::var_os(HELPER_ENV).is_none() {
        return Ok(());
    }

    let (mut backend, registry, session_id, window_id, pane) = create_embedded_session()?;
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
    let directory = tempfile::tempdir()?;
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

fn create_embedded_session() -> Result<(
    RmuxBackend,
    std::sync::Arc<MuxBackendRegistry>,
    String,
    String,
    bootty_mux::snapshot::MuxPaneAnchor,
)> {
    start_embedded_rmux_daemon_for_tests()?;
    bootty_rmux::link();
    let registry = std::sync::Arc::new(MuxBackendRegistry::collect([MuxBackendKind::Rmux])?);
    let session_id = format!("bootty-mux-contract-{}", std::process::id());
    let mut backend = RmuxBackend::new();
    backend.execute(MuxCommand::CreateProjectSession {
        session_id: session_id.clone(),
        cwd: std::env::temp_dir().to_string_lossy().into_owned(),
        tag: MuxSessionTag {
            identity: Some(new_session_identity()),
            space: None,
        },
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
        std::slice::from_ref(pane),
        Some(pane),
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
    // The budget bounds a genuine hang. It stays far above the scheduler jitter that a fully
    // parallel test run adds to a pane spawn.
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
