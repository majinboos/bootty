use bootty_mux::{
    MuxBackendKind, MuxBindingConfig,
    capability::BindingOperation,
    command::MuxCommand,
    config::{build_backend, selected_backend, unavailable_backend},
    controller::{BindingId, MuxScope, SpaceId},
};

#[test]
fn configured_backends_resolve_without_cross_backend_fallback() {
    for backend in [
        MuxBackendKind::Native,
        MuxBackendKind::Rmux,
        MuxBackendKind::Tmux,
        MuxBackendKind::Zellij,
    ] {
        let config = MuxBindingConfig {
            backend,
            ..Default::default()
        };
        let expected = if cfg!(windows) && backend == MuxBackendKind::Tmux {
            MuxBackendKind::Native
        } else {
            backend
        };

        assert_eq!(selected_backend(&config), expected);
    }
}

#[cfg(windows)]
#[test]
fn windows_keeps_remote_tmux_and_replaces_only_local_tmux() {
    let local = MuxBindingConfig {
        backend: MuxBackendKind::Tmux,
        ..Default::default()
    };
    let remote = MuxBindingConfig {
        backend: MuxBackendKind::Tmux,
        remote: Some(bootty_mux::SshTarget::for_host("host")),
        ..Default::default()
    };

    assert_eq!(selected_backend(&local), MuxBackendKind::Native);
    assert_eq!(selected_backend(&remote), MuxBackendKind::Tmux);
}

#[test]
fn built_backends_publish_the_exact_capability_matrix() {
    let scope = MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(2));
    for (backend, expected) in [
        (
            MuxBackendKind::Native,
            vec![
                BindingOperation::ActivateWindow,
                BindingOperation::CreateWindow,
                BindingOperation::RenameWindow,
                BindingOperation::NavigateWindow,
                BindingOperation::MoveWindow,
                BindingOperation::SplitPane,
                BindingOperation::NavigatePane,
                BindingOperation::ClosePane,
                BindingOperation::CreateProjectSession,
                BindingOperation::CreateWorktreeSession,
                BindingOperation::RenameSession,
                BindingOperation::DitchSession,
            ],
        ),
        (
            MuxBackendKind::Rmux,
            vec![
                BindingOperation::ActivateWindow,
                BindingOperation::CreateWindow,
                BindingOperation::RenameWindow,
                BindingOperation::NavigateWindow,
                BindingOperation::MoveWindow,
                BindingOperation::SplitPane,
                BindingOperation::ClosePane,
                BindingOperation::CreateProjectSession,
                BindingOperation::CreateWorktreeSession,
                BindingOperation::RenameSession,
                BindingOperation::DitchSession,
            ],
        ),
        (
            MuxBackendKind::Tmux,
            vec![
                BindingOperation::ActivateWindow,
                BindingOperation::CreateWindow,
                BindingOperation::RenameWindow,
                BindingOperation::NavigateWindow,
                BindingOperation::MoveWindow,
                BindingOperation::SplitPane,
                BindingOperation::NavigatePane,
                BindingOperation::ClosePane,
                BindingOperation::TogglePaneZoom,
                BindingOperation::CreateProjectSession,
                BindingOperation::CreateWorktreeSession,
                BindingOperation::RenameSession,
                BindingOperation::DitchSession,
            ],
        ),
        (
            MuxBackendKind::Zellij,
            vec![
                BindingOperation::CreateProjectSession,
                BindingOperation::CreateWorktreeSession,
                BindingOperation::RenameSession,
                BindingOperation::DitchSession,
            ],
        ),
    ] {
        let descriptor = build_backend(&MuxBindingConfig {
            backend,
            ..Default::default()
        })
        .capabilities(scope);

        assert_eq!(descriptor.scope(), scope);
        assert_eq!(descriptor.operations().collect::<Vec<_>>(), expected);
    }
}

#[test]
fn unavailable_backend_never_reads_or_mutates_local_state() {
    let mut backend = unavailable_backend("SSH profile 'lab' is unavailable");

    assert_eq!(
        backend.snapshot().unwrap_err().to_string(),
        "SSH profile 'lab' is unavailable"
    );
    assert_eq!(
        backend
            .execute(MuxCommand::DitchSession {
                session_id: "local".to_owned(),
            })
            .unwrap_err()
            .to_string(),
        "SSH profile 'lab' is unavailable"
    );
}
