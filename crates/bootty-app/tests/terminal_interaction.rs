#![cfg(unix)]

use pretty_assertions::assert_eq;

use std::{
    os::unix::fs::PermissionsExt,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use assert_fs::prelude::*;
use bootty_app::{
    AppEffect, AppState, ModalDialog,
    ui::{
        new_session_picker::NewSessionPickerEvent,
        terminal_find::{TerminalFindDialog, TerminalFindEvent},
    },
};
use bootty_command::{
    AppCommandRequest, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
};
use bootty_config::config::{BoottyConfig, MultiplexerBackendConfig, load_config_from_path};
use bootty_terminal::terminal_engine::TerminalSearchDirection;

#[path = "support/frames.rs"]
mod frames;
mod support;

/// Bounds a real pane-process hang without treating scheduler jitter as failure.
const PANE_BUDGET: Duration = Duration::from_secs(30);

fn native_state() -> (assert_fs::TempDir, AppState) {
    let directory = assert_fs::TempDir::new().expect("temporary app directory");
    let script = directory.child("terminal-interaction-shell");
    script
        .write_str("#!/bin/sh\nprintf '%s\\n' ready\nwhile IFS= read -r line; do\n  printf 'seen:%s\\n' \"$line\"\ndone\n")
        .expect("write terminal program");
    std::fs::set_permissions(script.path(), std::fs::Permissions::from_mode(0o755))
        .expect("make terminal program executable");
    let mut config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        multiplexer: bootty_config::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Native,
            ..bootty_config::config::MultiplexerConfig::default()
        },
        ..BoottyConfig::default()
    };
    config.session.shell = Some(script.path().to_string_lossy().into_owned());
    let state = AppState::new(config, support::backends(), Arc::new(|| {}), None, None)
        .expect("native app state");
    (directory, state)
}

fn submit(state: &mut AppState, action: &str) -> Option<CommandOutcome> {
    let started = Instant::now();
    let (response, outcomes) = mpsc::channel();
    state
        .app_command_sender(Caller::Socket)
        .try_send(AppCommandRequest {
            invocation: CommandInvocation::from_action(action, Caller::Socket),
            deadline: started + PANE_BUDGET,
            cancellation: CommandCancellation::new(),
            response,
        })
        .expect("submit command");
    (0..200).find_map(|tick| {
        state.update_frame(frames::frame(
            started + Duration::from_millis(tick),
            Vec::new(),
        ));
        outcomes.try_recv().ok()
    })
}

fn start_two_panes(state: &mut AppState) -> (String, String) {
    assert!(matches!(
        submit(state, "new_mux_session"),
        Some(CommandOutcome::Success { .. })
    ));
    assert!(matches!(
        state.modal_dialog(),
        Some(ModalDialog::NewSession(_))
    ));
    state.apply_picker_event(NewSessionPickerEvent::CreateSession {
        cwd: std::env::temp_dir().to_string_lossy().into_owned(),
    });
    let started = Instant::now();
    for tick in 0..20 {
        state.update_frame(frames::frame(
            started + Duration::from_millis(250 + tick),
            Vec::new(),
        ));
    }
    assert!(matches!(
        submit(state, "split_right"),
        Some(CommandOutcome::Success { .. })
    ));
    let focused = state.focused_pane().expect("focused native pane");
    let other = state
        .pane_rects(
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(200.0, 100.0)),
            4.0,
        )
        .into_iter()
        .map(|(pane_id, _)| pane_id)
        .find(|pane_id| pane_id != &focused)
        .expect("other native pane");
    (other, focused)
}

fn wait_for_pane_text(state: &mut AppState, pane_id: &str, expected: &str) {
    let deadline = Instant::now() + PANE_BUDGET;
    while Instant::now() < deadline {
        state.update_frame(frames::frame(Instant::now(), Vec::new()));
        if let Some(runtime) = state.terminal_mut().focused_terminal_runtime(pane_id) {
            if runtime
                .extract_frame()
                .ok()
                .is_some_and(|frame| frame.text_rows().iter().any(|row| row.contains(expected)))
            {
                return;
            }
            assert!(
                !runtime.child_exited().unwrap_or(false),
                "pane {pane_id} shell exited before it rendered {expected:?}"
            );
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("pane {pane_id} did not render {expected:?}");
}

fn search_state(state: &mut AppState, pane_id: &str) -> (usize, bool) {
    let frame = state
        .terminal_mut()
        .focused_terminal_runtime(pane_id)
        .expect("pane runtime")
        .extract_frame()
        .expect("pane frame");
    (
        frame.search_match_count,
        frame.active_search_match.is_some(),
    )
}

#[test]
fn normal_search_targets_focused_pane_and_close_clears_search() {
    let (_directory, mut state) = native_state();
    let (unfocused_pane, focused_pane) = start_two_panes(&mut state);

    state
        .terminal_mut()
        .focused_terminal_runtime(&unfocused_pane)
        .expect("first pane")
        .write_input(b"unfocused-only\n")
        .expect("write first pane marker");
    state
        .terminal_mut()
        .focused_terminal_runtime(&focused_pane)
        .expect("second pane")
        .write_input(b"focused-only\n")
        .expect("write second pane marker");
    wait_for_pane_text(&mut state, &unfocused_pane, "seen:unfocused-only");
    wait_for_pane_text(&mut state, &focused_pane, "seen:focused-only");
    state.focus_pane(&focused_pane);

    let dialog = TerminalFindDialog::open(String::new());
    state.apply_terminal_find_event(
        dialog,
        TerminalFindEvent::Search {
            query: "focused-only".to_owned(),
            direction: TerminalSearchDirection::Current,
        },
    );
    assert!(search_state(&mut state, &focused_pane).0 > 0);
    assert_eq!(search_state(&mut state, &unfocused_pane).0, 0);

    let dialog = state.take_terminal_find_dialog().expect("find dialog");
    state.apply_terminal_find_event(dialog, TerminalFindEvent::Close);
    assert_eq!(search_state(&mut state, &focused_pane), (0, false));
}

#[test]
fn copy_mode_search_returns_focus_and_terminal_text_reaches_shell() {
    let (_directory, mut state) = native_state();
    let (_unfocused_pane, focused_pane) = start_two_panes(&mut state);
    wait_for_pane_text(&mut state, &focused_pane, "ready");
    state.terminal_mut().enter_copy_mode().expect("copy mode");

    state.update_frame(frames::frame(
        Instant::now(),
        vec![egui::Event::Key {
            key: egui::Key::Slash,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
    ));
    let dialog = state.take_terminal_find_dialog().expect("search dialog");
    state.apply_terminal_find_event(
        dialog,
        TerminalFindEvent::Search {
            query: "ready".to_owned(),
            direction: TerminalSearchDirection::Next,
        },
    );
    assert!(state.terminal_focused());

    state.update_frame(frames::frame(
        Instant::now(),
        vec![
            egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::Text("after-search\n".to_owned()),
        ],
    ));
    assert!(
        !state
            .terminal_mut()
            .copy_mode_active()
            .expect("copy mode state")
    );

    wait_for_pane_text(&mut state, &focused_pane, "seen:after-search");
}

#[test]
fn opening_another_overlay_clears_terminal_search() {
    let (_directory, mut state) = native_state();
    let (_unfocused_pane, focused_pane) = start_two_panes(&mut state);
    wait_for_pane_text(&mut state, &focused_pane, "ready");

    assert!(matches!(
        submit(&mut state, "start_search"),
        Some(CommandOutcome::Success { .. })
    ));

    let dialog = state.take_terminal_find_dialog().expect("find dialog");
    state.apply_terminal_find_event(
        dialog,
        TerminalFindEvent::Search {
            query: "ready".to_owned(),
            direction: TerminalSearchDirection::Current,
        },
    );
    assert!(search_state(&mut state, &focused_pane).0 > 0);

    assert!(matches!(
        submit(&mut state, "command_palette"),
        Some(CommandOutcome::Success { .. })
    ));

    assert_eq!(search_state(&mut state, &focused_pane), (0, false));
}

#[test]
fn native_terminal_progress_updates_active_binding_presentation() {
    let directory = assert_fs::TempDir::new().expect("temporary app directory");
    let script = directory.child("terminal-side-effects");
    script
        .write_str("#!/bin/sh\nprintf '\\033]9;4;42\\033\\\\'\nsleep 1\n")
        .expect("write terminal program");
    std::fs::set_permissions(script.path(), std::fs::Permissions::from_mode(0o755))
        .expect("make terminal program executable");

    let config_file = directory.child("config.toml");
    config_file
        .write_str("[multiplexer]\nbackend = \"native\"\n")
        .expect("write config");
    let mut config = load_config_from_path(config_file.path()).expect("load config");
    config.session.shell = Some(script.path().to_string_lossy().into_owned());
    let mut state = AppState::new(config, support::backends(), Arc::new(|| {}), None, None)
        .expect("start app state");
    let pane = bootty_mux::snapshot::MuxPaneAnchor {
        session_id: "facts".to_owned(),
        pane_id: Some("%1".to_owned()),
        cwd: None,
        pane_pid: None,
        process: None,
    };
    state
        .terminal_mut()
        .sync_native_window(
            std::slice::from_ref(&pane),
            Some(&pane),
            Some("window"),
            MultiplexerBackendConfig::Native,
            false,
        )
        .expect("start terminal program");

    let deadline = Instant::now() + PANE_BUDGET;
    let mut observed_progress_repaint = false;
    while Instant::now() < deadline && !observed_progress_repaint {
        for effect in state.update_frame(frames::frame(Instant::now(), Vec::new())) {
            observed_progress_repaint |= effect == AppEffect::RequestRepaint;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        observed_progress_repaint,
        "terminal progress must update binding presentation state"
    );
}
