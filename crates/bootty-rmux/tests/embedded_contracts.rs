#![cfg(unix)]

use anyhow::Result;
use bootty_identity::ApplicationIdentity;
use bootty_mux::{command::MuxCommand, provider::MuxBackendRegistry, terminal::ActiveTerminal};
use bootty_mux_model::{MuxBackendKind, MuxBindingConfig};
use bootty_rmux::{RmuxBackend, endpoint_path_for, start_embedded_rmux_daemon_for_tests};
use bootty_runtime::{frame_source::TerminalFrameSource, terminal_session::TerminalSessionConfig};
use bootty_surface::geometry::TerminalGeometry;

const HELPER_ENV: &str = "BOOTTY_RMUX_EMBEDDED_CONTRACT_HELPER";
const ISOLATED_PATH: &str = "/usr/bin:/bin";

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
    backend.execute(MuxCommand::CreateProjectSession {
        session_id: session_id.clone(),
        cwd: std::env::temp_dir().to_string_lossy().into_owned(),
    })?;

    let snapshot = backend.snapshot()?;
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("created rmux session");
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

fn run_embedded_helper(name: &str) -> Result<()> {
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
