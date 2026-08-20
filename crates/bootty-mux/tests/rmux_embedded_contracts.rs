#![cfg(unix)]

use anyhow::Result;
use bootty_mux::{
    MuxBackendKind, MuxBindingConfig, command::MuxCommand, rmux::RmuxBackend,
    start_embedded_rmux_daemon_for_tests, terminal::ActiveTerminal,
};
use bootty_runtime::{frame_source::TerminalFrameSource, terminal_session::TerminalSessionConfig};
use bootty_surface::geometry::TerminalGeometry;

const HELPER_ENV: &str = "BOOTTY_RMUX_EMBEDDED_CONTRACT_HELPER";

#[test]
fn embedded_rmux_owns_session_lifecycle_without_an_external_executable() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let status = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "embedded_rmux_owns_session_lifecycle_without_an_external_executable_helper",
        ])
        .env(HELPER_ENV, "1")
        .env("RMUX_TMPDIR", directory.path())
        .env("BOOTTY_APPLICATION_IDENTITY", "bootty")
        .status()?;

    assert!(status.success());
    Ok(())
}

#[test]
fn embedded_rmux_owns_session_lifecycle_without_an_external_executable_helper() -> Result<()> {
    if std::env::var_os(HELPER_ENV).is_none() {
        return Ok(());
    }

    start_embedded_rmux_daemon_for_tests()?;
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

    let mut terminal = ActiveTerminal::new(
        TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        },
        &MuxBindingConfig {
            backend: MuxBackendKind::Rmux,
            ..MuxBindingConfig::default()
        },
        TerminalSessionConfig::default(),
        std::sync::Arc::new(|| {}),
    );
    terminal.sync_native_window(
        std::slice::from_ref(&pane),
        Some(&pane),
        Some(&window.id),
        MuxBackendKind::Rmux,
        false,
    )?;
    terminal.write_input(b"printf 'BOOTTY_RMUX_FRAME\\n'\r")?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        terminal.drain_pty();
        let frame = terminal.extract_frame()?;
        if frame
            .text
            .iter()
            .collect::<String>()
            .contains("BOOTTY_RMUX_FRAME")
        {
            break;
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "rmux terminal did not publish pane output"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

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
