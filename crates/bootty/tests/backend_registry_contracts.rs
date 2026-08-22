use bootty_mux::{
    MuxBackendKind, MuxBindingConfig, capability::BindingOperation, controller::SpaceId,
    provider::MuxCommandDispatch,
};

mod support;

#[test]
fn configured_backends_resolve_without_cross_backend_fallback() {
    let registry = support::backends();
    for backend in [
        MuxBackendKind::Native,
        MuxBackendKind::Rmux,
        MuxBackendKind::Tmux,
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

        assert_eq!(registry.selected_kind(&config), expected);
    }
}

#[test]
fn providers_publish_their_command_dispatch() {
    let registry = support::backends();
    let native = MuxBindingConfig {
        backend: MuxBackendKind::Native,
        ..Default::default()
    };
    let rmux = MuxBindingConfig {
        backend: MuxBackendKind::Rmux,
        ..Default::default()
    };

    assert_eq!(
        registry.command_dispatch(&native),
        MuxCommandDispatch::CallerThread
    );
    assert_eq!(
        registry.command_dispatch(&rmux),
        MuxCommandDispatch::WorkerThread
    );
}

#[cfg(windows)]
#[test]
fn windows_keeps_remote_tmux_and_replaces_only_local_tmux() {
    let registry = support::backends();
    let local = MuxBindingConfig {
        backend: MuxBackendKind::Tmux,
        ..Default::default()
    };
    let remote = MuxBindingConfig {
        backend: MuxBackendKind::Tmux,
        remote: Some(bootty_mux::SshTarget::for_host("host")),
        ..Default::default()
    };

    assert_eq!(registry.selected_kind(&local), MuxBackendKind::Native);
    assert_eq!(registry.selected_kind(&remote), MuxBackendKind::Tmux);
}

#[test]
fn built_backends_publish_the_exact_capability_matrix() {
    let registry = support::backends();
    let scope = SpaceId::from_persistence(1);
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
                BindingOperation::StampSession,
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
                BindingOperation::StampSession,
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
                BindingOperation::StampSession,
            ],
        ),
    ] {
        let descriptor = registry.capabilities(
            &MuxBindingConfig {
                backend,
                ..Default::default()
            },
            scope,
        );

        assert_eq!(descriptor.scope(), scope);
        assert_eq!(descriptor.operations().collect::<Vec<_>>(), expected);
    }
}
