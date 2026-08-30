use bootty_mux::{
    MuxBackendKind, MuxBindingConfig, capability::BindingOperation, controller::SpaceId,
    provider::MuxCommandDispatch,
};
use pretty_assertions::assert_eq;
use rstest::rstest;

mod support;

#[rstest]
#[case::herdr(MuxBackendKind::Herdr)]
#[case::native(MuxBackendKind::Native)]
#[case::rmux(MuxBackendKind::Rmux)]
#[case::tmux(MuxBackendKind::Tmux)]
fn configured_backends_resolve_without_cross_backend_fallback(#[case] backend: MuxBackendKind) {
    let registry = support::backends();
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

#[rstest]
#[case::herdr(MuxBackendKind::Herdr, MuxCommandDispatch::WorkerThread)]
#[case::native(MuxBackendKind::Native, MuxCommandDispatch::CallerThread)]
#[case::rmux(MuxBackendKind::Rmux, MuxCommandDispatch::WorkerThread)]
fn providers_publish_their_command_dispatch(
    #[case] backend: MuxBackendKind,
    #[case] expected: MuxCommandDispatch,
) {
    let registry = support::backends();
    let config = MuxBindingConfig {
        backend,
        ..Default::default()
    };

    assert_eq!(registry.command_dispatch(&config), expected);
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

#[rstest]
#[case::herdr(MuxBackendKind::Herdr)]
#[case::native(MuxBackendKind::Native)]
#[case::rmux(MuxBackendKind::Rmux)]
#[case::tmux(MuxBackendKind::Tmux)]
fn built_backends_publish_the_exact_capability_matrix(#[case] backend: MuxBackendKind) {
    let registry = support::backends();
    let scope = SpaceId::from_persistence(1);
    let mut expected = vec![
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
    ];
    if matches!(
        backend,
        MuxBackendKind::Herdr | MuxBackendKind::Rmux | MuxBackendKind::Tmux
    ) {
        expected.insert(8, BindingOperation::TogglePaneZoom);
    }
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
