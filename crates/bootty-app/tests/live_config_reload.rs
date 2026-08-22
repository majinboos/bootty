use pretty_assertions::{assert_eq, assert_ne};

use std::sync::Arc;
#[cfg(unix)]
use std::time::{Duration, Instant};

use assert_fs::{TempDir, fixture::ChildPath, prelude::*};
use bootty_app::AppState;
use bootty_config::{
    color::Color,
    config::{BoottyConfig, CursorStyleConfig, MultiplexerBackendConfig, load_config_from_path},
};
use bootty_mux::snapshot::MuxPaneAnchor;
use eframe::egui;

#[path = "support/events.rs"]
mod events;
#[path = "support/frames.rs"]
mod frames;
mod support;

fn app_state(config: BoottyConfig) -> AppState {
    AppState::new(config, support::backends(), Arc::new(|| {}), None, None)
        .expect("start app state")
}

fn state_from_config(source: &str) -> (TempDir, ChildPath, AppState) {
    let directory = TempDir::new().expect("temporary config directory");
    let config_file = directory.child("config.toml");
    config_file.write_str(source).expect("write initial config");
    let config = load_config_from_path(config_file.path()).expect("load initial config");
    let state = app_state(config);
    (directory, config_file, state)
}

#[test]
fn selecting_a_missing_space_is_a_noop() {
    let directory = assert_fs::TempDir::new().expect("temporary app directory");
    let mut config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        ..BoottyConfig::default()
    };
    config
        .input
        .keybind
        .push("ctrl+3=select_space:3".to_owned());
    let mut state = app_state(config);

    state.update_frame(frames::frame(
        std::time::Instant::now(),
        vec![events::key_event(
            egui::Key::Num3,
            egui::Modifiers {
                ctrl: true,
                ..egui::Modifiers::NONE
            },
        )],
    ));

    assert_eq!(state.last_error(), None);
}

#[test]
fn a_failed_app_write_keeps_the_error_visible() {
    let (_directory, config_file, mut state) =
        state_from_config("[chrome]\nsidebar-width = 320\n\n[multiplexer]\nbackend = \"rmux\"\n");
    std::fs::remove_file(config_file.path()).expect("remove config file");
    std::fs::create_dir(config_file.path()).expect("replace config with directory");

    state.set_sidebar_width_live(444.0);
    state.persist_sidebar_width(444.0, &mut Vec::new());

    assert_eq!(state.config().chrome.sidebar_width, 444.0);
    assert!(
        state
            .last_error()
            .is_some_and(|error| error.contains("config file"))
    );
    assert!(config_file.path().is_dir());
}

#[test]
fn every_accepted_config_change_advances_the_revision() {
    let (_directory, _config_file, mut state) =
        state_from_config("[chrome]\nsidebar-width = 320\n");

    let initial = state.config_revision();
    state.set_sidebar_width_live(444.0);
    let after_live = state.config_revision();
    assert_ne!(after_live, initial, "a live edit is a config change");

    state.persist_sidebar_width(444.0, &mut Vec::new());
    assert_ne!(
        state.config_revision(),
        after_live,
        "an accepted document is a config change"
    );
}

#[test]
fn live_terminal_policy_reload_accepts_one_complete_candidate() {
    let (_directory, config_file, mut state) = state_from_config("[appearance]\nmode = \"dark\"\n");

    config_file
        .write_str(
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

#[test]
fn live_reload_does_not_require_a_restart() {
    let (_directory, config_file, mut state) = state_from_config("[chrome]\nsidebar = true\n");

    config_file
        .write_str("[chrome]\nsidebar = false\n")
        .expect("write live chrome change");
    assert!(state.reload_config(&mut Vec::new()));
    assert_eq!(state.last_error(), None);
}

#[test]
fn every_new_window_policy_reports_the_restart_requirement() {
    for (name, source) in [
        ("session", "[session]\nshell = \"/bin/sh\"\n"),
        ("size", "[window]\nwidth = 900\n"),
        ("fullscreen", "[window]\nfullscreen = \"non-native\"\n"),
        ("decoration", "[window]\nwindow-decoration = \"none\"\n"),
        ("titlebar", "[window]\nmacos-titlebar-style = \"hidden\"\n"),
    ] {
        let (_directory, config_file, mut state) = state_from_config("");

        config_file
            .write_str(source)
            .expect("write new-window change");
        assert!(state.reload_config(&mut Vec::new()), "{name}");
        assert_eq!(
            state.last_error().as_deref(),
            Some("config reloaded; session/window settings require a new window or restart"),
            "{name}",
        );
    }
}

#[cfg(unix)]
#[test]
fn a_dead_terminal_warns_after_acceptance_and_new_panes_use_the_accepted_config() {
    let (_directory, config_file, mut state) = state_from_config(
        "[multiplexer]\nbackend = \"native\"\n\n[session]\nshell = \"/bootty/missing-shell\"\n",
    );
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

    config_file
        .write_str(
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
            .is_some_and(|error| error.contains("terminal config publication failed for SpaceId"))
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
    // The budget bounds a genuine hang. It stays far above the scheduler jitter that a fully
    // parallel test run adds to a pane spawn.
    let deadline = Instant::now() + Duration::from_secs(30);
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
