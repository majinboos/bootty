#![cfg(unix)]

use std::{
    os::unix::fs::PermissionsExt,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use bootty_app::{
    app::{AppState, FrameInputs, ModalDialog, ViewportSnapshot},
    commands::{AppCommandRequest, Caller, CommandCancellation, CommandInvocation, CommandOutcome},
    config::{BoottyConfig, MultiplexerBackendConfig},
    geometry::ViewTransform,
    renderer::RendererMetrics,
    terminal::TerminalSearchDirection,
    ui::{
        new_session_picker::NewSessionPickerEvent,
        terminal_find::{TerminalFindDialog, TerminalFindEvent},
    },
};

mod support;

fn frame(now: Instant, events: Vec<egui::Event>) -> FrameInputs {
    FrameInputs {
        now,
        events,
        dropped_file_paths: Vec::new(),
        modifiers: egui::Modifiers::NONE,
        hover_pos: None,
        pressed_mouse_button: None,
        viewport: ViewportSnapshot::default(),
        window_focused: true,
        renderer_metrics: RendererMetrics::default(),
        terminal_cell_width: 9.0,
        terminal_cell_height: 20.0,
        terminal_scale_factor: 1.0,
        terminal_view_transform: ViewTransform::IDENTITY,
    }
}

fn native_state() -> (tempfile::TempDir, AppState) {
    let directory = tempfile::tempdir().expect("temporary app directory");
    let script = directory.path().join("terminal-interaction-shell");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '%s\\n' ready\nwhile IFS= read -r line; do\n  printf 'seen:%s\\n' \"$line\"\ndone\n",
    )
    .expect("write terminal program");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("make terminal program executable");
    let mut config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        multiplexer: bootty_app::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Native,
            ..bootty_app::config::MultiplexerConfig::default()
        },
        ..BoottyConfig::default()
    };
    config.session.shell = Some(script.to_string_lossy().into_owned());
    let state = AppState::new(config, support::backends(), Arc::new(|| {}), None, None)
        .expect("native app state");
    (directory, state)
}

fn start_two_panes(state: &mut AppState) -> (String, String) {
    let started = Instant::now();
    let commands = state.app_command_sender(Caller::Socket);
    let (response, outcomes) = mpsc::channel();
    commands
        .try_send(AppCommandRequest {
            invocation: CommandInvocation::from_action("new_mux_session", Caller::Socket),
            deadline: started + Duration::from_secs(2),
            cancellation: CommandCancellation::new(),
            response,
        })
        .expect("submit session");
    let outcome = (0..200).find_map(|tick| {
        state.update_frame(frame(started + Duration::from_millis(tick), Vec::new()));
        outcomes.try_recv().ok()
    });
    assert!(matches!(outcome, Some(CommandOutcome::Success { .. })));
    let ModalDialog::NewSession(dialog) = state.take_modal_dialog().expect("session dialog") else {
        panic!("expected session dialog")
    };
    state.apply_picker_event(
        dialog,
        NewSessionPickerEvent::CreateSession {
            cwd: std::env::temp_dir().to_string_lossy().into_owned(),
        },
    );
    for tick in 0..20 {
        state.update_frame(frame(
            started + Duration::from_millis(250 + tick),
            Vec::new(),
        ));
    }
    let commands = state.app_command_sender(Caller::Socket);
    let (response, outcomes) = mpsc::channel();
    commands
        .try_send(AppCommandRequest {
            invocation: CommandInvocation::from_action("split_right", Caller::Socket),
            deadline: Instant::now() + Duration::from_secs(2),
            cancellation: CommandCancellation::new(),
            response,
        })
        .expect("submit split");
    let outcome = (0..200).find_map(|tick| {
        state.update_frame(frame(
            Instant::now() + Duration::from_millis(tick),
            Vec::new(),
        ));
        outcomes.try_recv().ok()
    });
    assert!(matches!(outcome, Some(CommandOutcome::Success { .. })));
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
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        state.update_frame(frame(Instant::now(), Vec::new()));
        if state
            .terminal_mut()
            .focused_terminal_runtime(pane_id)
            .and_then(|runtime| runtime.extract_frame().ok())
            .is_some_and(|frame| frame.text_rows().iter().any(|row| row.contains(expected)))
        {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("pane {pane_id} did not render {expected:?}");
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
    let focused = state
        .terminal_mut()
        .focused_terminal_runtime(&focused_pane)
        .expect("focused pane")
        .extract_frame()
        .expect("focused frame");
    assert!(focused.search_match_count > 0);
    let unfocused = state
        .terminal_mut()
        .focused_terminal_runtime(&unfocused_pane)
        .expect("unfocused pane")
        .extract_frame()
        .expect("unfocused frame");
    assert_eq!(unfocused.search_match_count, 0);

    let dialog = state.take_terminal_find_dialog().expect("find dialog");
    state.apply_terminal_find_event(dialog, TerminalFindEvent::Close);
    let cleared = state
        .terminal_mut()
        .focused_terminal_runtime(&focused_pane)
        .expect("focused pane")
        .extract_frame()
        .expect("cleared frame");
    assert_eq!(cleared.search_match_count, 0);
    assert!(cleared.active_search_match.is_none());
}

#[test]
fn copy_mode_search_returns_focus_and_terminal_text_reaches_shell() {
    let (_directory, mut state) = native_state();
    let (_unfocused_pane, focused_pane) = start_two_panes(&mut state);
    wait_for_pane_text(&mut state, &focused_pane, "ready");
    state.terminal_mut().enter_copy_mode().expect("copy mode");

    state.update_frame(frame(
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

    state.update_frame(frame(
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

    let started = Instant::now();
    let commands = state.app_command_sender(Caller::Socket);
    let (response, outcomes) = mpsc::channel();
    commands
        .try_send(AppCommandRequest {
            invocation: CommandInvocation::from_action("start_search", Caller::Socket),
            deadline: started + Duration::from_secs(2),
            cancellation: CommandCancellation::new(),
            response,
        })
        .expect("submit search command");
    let outcome = (0..200).find_map(|tick| {
        state.update_frame(frame(started + Duration::from_millis(tick), Vec::new()));
        outcomes.try_recv().ok()
    });
    assert!(matches!(outcome, Some(CommandOutcome::Success { .. })));

    let dialog = state.take_terminal_find_dialog().expect("find dialog");
    state.apply_terminal_find_event(
        dialog,
        TerminalFindEvent::Search {
            query: "ready".to_owned(),
            direction: TerminalSearchDirection::Current,
        },
    );
    let searched = state
        .terminal_mut()
        .focused_terminal_runtime(&focused_pane)
        .expect("focused pane")
        .extract_frame()
        .expect("searched frame");
    assert!(searched.search_match_count > 0);

    let commands = state.app_command_sender(Caller::Socket);
    let (response, outcomes) = mpsc::channel();
    commands
        .try_send(AppCommandRequest {
            invocation: CommandInvocation::from_action("command_palette", Caller::Socket),
            deadline: Instant::now() + Duration::from_secs(2),
            cancellation: CommandCancellation::new(),
            response,
        })
        .expect("submit overlay command");
    let outcome = (0..200).find_map(|tick| {
        state.update_frame(frame(
            Instant::now() + Duration::from_millis(tick),
            Vec::new(),
        ));
        outcomes.try_recv().ok()
    });
    assert!(matches!(outcome, Some(CommandOutcome::Success { .. })));

    let cleared = state
        .terminal_mut()
        .focused_terminal_runtime(&focused_pane)
        .expect("focused pane")
        .extract_frame()
        .expect("cleared frame");
    assert_eq!(cleared.search_match_count, 0);
    assert!(cleared.active_search_match.is_none());
}
