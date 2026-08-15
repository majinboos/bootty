use bootty_mux::{
    MuxBackendKind, MuxBindingConfig,
    command::{MuxCommand, MuxSplitDirection},
    provider::MuxBackendRegistry,
};
use bootty_mux_model::SshTarget;
use bootty_remote::ssh::SshRemote;
use bootty_rmux::RemoteRmuxRequest;

#[test]
fn remote_rmux_request_round_trips_hostile_backend_arguments() {
    let request = RemoteRmuxRequest::Execute {
        command: MuxCommand::SplitPane {
            session_id: "session ; $HOME".to_owned(),
            pane_id: Some("pane with 'quotes'".to_owned()),
            direction: MuxSplitDirection::Down,
        },
    };

    let payload = request.encode().expect("encode request");
    assert_eq!(RemoteRmuxRequest::decode(&payload).unwrap(), request);
    assert!(
        payload
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    );
}

#[test]
fn remote_rmux_protocol_preserves_pane_stream_and_resize_values() {
    for request in [
        RemoteRmuxRequest::PaneStream {
            session: "project".to_owned(),
            pane: "%7".to_owned(),
            max_scrollback: 320_000,
        },
        RemoteRmuxRequest::PaneInput {
            session: "project".to_owned(),
            pane: "%7".to_owned(),
        },
        RemoteRmuxRequest::Resize {
            session: "project".to_owned(),
            pane: "%7".to_owned(),
            cols: 137,
            rows: 51,
        },
    ] {
        let payload = request.encode().unwrap();
        assert_eq!(RemoteRmuxRequest::decode(&payload).unwrap(), request);
    }
}

#[test]
fn remote_rmux_rejects_invalid_and_oversized_payloads() {
    assert!(RemoteRmuxRequest::decode("not-base64!").is_err());
    assert!(RemoteRmuxRequest::decode(&"A".repeat(2 * 1024 * 1024 + 1)).is_err());
}

#[test]
fn remote_rmux_backend_builds() {
    bootty_rmux::link();
    let remote = SshRemote::new(SshTarget::for_host("devbox"));
    let config = MuxBindingConfig {
        backend: MuxBackendKind::Rmux,
        remote: Some(remote.target().clone()),
        ..MuxBindingConfig::default()
    };
    let registry =
        MuxBackendRegistry::collect([MuxBackendKind::Rmux]).expect("collect rmux backend provider");
    let backend = registry.build_backend(&config, None);
    let _ = backend;
}
