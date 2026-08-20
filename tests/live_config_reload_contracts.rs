use std::sync::Arc;
#[cfg(unix)]
use std::time::{Duration, Instant};

use bootty_app::{
    app::AppState,
    color::Color,
    config::{CursorStyleConfig, MultiplexerBackendConfig, load_config_from_path},
    mux::snapshot::MuxPaneAnchor,
};

#[test]
fn live_terminal_policy_reload_accepts_one_complete_candidate() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let config_path = directory.path().join("config.toml");
    std::fs::write(&config_path, "[appearance]\nmode = \"dark\"\n").expect("write initial config");
    let config = load_config_from_path(&config_path).expect("load initial config");
    let mut state = AppState::new(config, Arc::new(|| {}), None, None).expect("start app state");

    std::fs::write(
        &config_path,
        r##"
[appearance]
mode = "dark"

[appearance.dark.colors]
background = "#010203"

[cursor]
style = "hollow-block"
blink = false

[session]
glyph-protocol = false
"##,
    )
    .expect("write changed terminal policy");

    assert!(state.reload_config(&mut Vec::new()));
    assert_eq!(
        state.config().appearance.dark.colors.background,
        Some(Color {
            r: 1,
            g: 2,
            b: 3,
            a: u8::MAX,
        })
    );
    assert_eq!(
        state.config().cursor.style,
        Some(CursorStyleConfig::HollowBlock)
    );
    assert_eq!(state.config().cursor.blink, Some(false));
    assert!(!state.config().session.glyph_protocol);
}

#[cfg(unix)]
#[test]
fn a_dead_terminal_warns_after_acceptance_and_new_panes_use_the_accepted_config() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let config_path = directory.path().join("config.toml");
    std::fs::write(
        &config_path,
        "[multiplexer]\nbackend = \"native\"\n\n[session]\nshell = \"/bootty/missing-shell\"\n",
    )
    .expect("write initial config");
    let config = load_config_from_path(&config_path).expect("load initial config");
    let mut state = AppState::new(config, Arc::new(|| {}), None, None).expect("start app state");
    let failed = pane("failed", "%1");
    state
        .terminal_mut()
        .sync_native_window(
            std::slice::from_ref(&failed),
            Some(&failed),
            Some("window"),
            MultiplexerBackendConfig::Native,
            false,
        )
        .expect("start failing pane");
    let failure = wait_for_startup_result(&mut state, "%1").expect_err("startup must fail");
    assert_eq!(failure, "spawn shell in PTY");

    std::fs::write(
        &config_path,
        r##"
[multiplexer]
backend = "native"

[appearance]
mode = "dark"

[appearance.dark.colors]
background = "#010203"

[cursor]
style = "hollow-block"
blink = false

[session]
shell = "/bin/sh"
glyph-protocol = false
"##,
    )
    .expect("write accepted config");

    assert!(state.reload_config(&mut Vec::new()));
    assert_eq!(state.config().session.shell.as_deref(), Some("/bin/sh"));
    assert!(
        state
            .last_error()
            .is_some_and(|error| error.contains("terminal config publication failed for MuxScope"))
    );

    state.terminal_mut().discard_active_pane();
    let replacement = pane("replacement", "%2");
    state
        .terminal_mut()
        .sync_native_window(
            std::slice::from_ref(&replacement),
            Some(&replacement),
            Some("window"),
            MultiplexerBackendConfig::Native,
            false,
        )
        .expect("start replacement pane");
    wait_for_startup_result(&mut state, "%2").expect("accepted shell starts replacement pane");
}

#[cfg(unix)]
fn pane(session_id: &str, pane_id: &str) -> MuxPaneAnchor {
    MuxPaneAnchor {
        session_id: session_id.to_owned(),
        pane_id: Some(pane_id.to_owned()),
        cwd: None,
        pane_pid: None,
        process: None,
    }
}

#[cfg(unix)]
fn wait_for_startup_result(state: &mut AppState, pane_id: &str) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let runtime = state
            .terminal_mut()
            .focused_terminal_runtime(pane_id)
            .expect("focused terminal runtime");
        runtime
            .current_working_directory()
            .map_err(|error| error.to_string())?;
        if runtime.tty_name().is_some() {
            return Ok(());
        }
        assert!(Instant::now() < deadline, "terminal startup timed out");
        std::thread::yield_now();
    }
}
