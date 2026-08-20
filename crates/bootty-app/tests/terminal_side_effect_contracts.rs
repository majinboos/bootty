#![cfg(unix)]

use std::{
    os::unix::fs::PermissionsExt,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use bootty_app::{
    app::{AppEffect, AppState, FrameInputs, ViewportSnapshot},
    config::{MultiplexerBackendConfig, load_config_from_path},
    geometry::ViewTransform,
    mux::snapshot::MuxPaneAnchor,
    renderer::RendererMetrics,
};

mod support;

fn frame(now: Instant) -> FrameInputs {
    FrameInputs {
        now,
        events: Vec::new(),
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

#[test]
fn native_terminal_progress_updates_active_binding_presentation() {
    let directory = tempfile::tempdir().expect("temporary app directory");
    let script = directory.path().join("terminal-side-effects");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '\\033]9;4;42\\033\\\\'\nsleep 1\n",
    )
    .expect("write terminal program");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("make terminal program executable");

    let config_path = directory.path().join("config.toml");
    std::fs::write(&config_path, "[multiplexer]\nbackend = \"native\"\n").expect("write config");
    let mut config = load_config_from_path(&config_path).expect("load config");
    config.session.shell = Some(script.to_string_lossy().into_owned());
    let mut state = AppState::new(config, support::backends(), Arc::new(|| {}), None, None)
        .expect("start app state");
    let pane = MuxPaneAnchor {
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

    // The budget bounds a genuine hang. It stays far above the scheduler jitter that a fully
    // parallel test run adds to a pane spawn.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut observed_progress_repaint = false;
    while Instant::now() < deadline && !observed_progress_repaint {
        for effect in state.update_frame(frame(Instant::now())) {
            observed_progress_repaint |= effect == AppEffect::RequestRepaint;
        }
        thread::sleep(Duration::from_millis(10));
    }

    assert!(
        observed_progress_repaint,
        "terminal progress must update binding presentation state"
    );
}
