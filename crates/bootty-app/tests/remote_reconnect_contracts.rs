use std::sync::Arc;

use bootty_app::{app::AppState, config::load_config_from_path};

#[test]
fn manual_reconnect_targets_only_a_remote_binding() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let local_path = directory.path().join("local.toml");
    std::fs::write(&local_path, "[multiplexer]\nbackend = \"native\"\n")
        .expect("write local config");
    let local = load_config_from_path(&local_path).expect("load local config");
    let mut local_state =
        AppState::new(local, Arc::new(|| {}), None, None).expect("start local state");
    assert!(!local_state.reconnect_space_from_ui(local_state.active_space_id()));

    let remote_path = directory.path().join("remote.toml");
    std::fs::write(
        &remote_path,
        r#"
[multiplexer]
backend = "tmux"

[multiplexer.remote]
host = "reconnect.test"
program = "/bootty/missing-ssh"
"#,
    )
    .expect("write remote config");
    let remote = load_config_from_path(&remote_path).expect("load remote config");
    let mut remote_state =
        AppState::new(remote, Arc::new(|| {}), None, None).expect("start remote state");
    let space_id = remote_state.active_space_id();

    assert!(remote_state.reconnect_space_from_ui(space_id));
    let summary = remote_state
        .space_summaries()
        .into_iter()
        .find(|space| space.id == space_id)
        .expect("active Space summary");
    assert_eq!(
        summary.error.as_deref(),
        Some("reconnecting to reconnect.test")
    );
}
