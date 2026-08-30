use bootty_mux::{
    MuxBackendKind, MuxBindingConfig, SshTarget,
    provider::{
        GeneratedSessionNamePolicy, MuxAppBackendPolicy, PaneBehavior, PaneTopology,
        PersistedSessionPolicy, SelectionPublicationPolicy, TerminalProgressPolicy,
        TerminalResidency,
    },
};
use pretty_assertions::assert_eq;
use rstest::rstest;

mod support;

fn binding(backend: MuxBackendKind) -> MuxBindingConfig {
    MuxBindingConfig {
        backend,
        ..MuxBindingConfig::default()
    }
}

#[rstest]
#[case::herdr(
    MuxBackendKind::Herdr,
    MuxAppBackendPolicy {
        panes: PaneBehavior { topology: PaneTopology::Attach, cache_terminals: true, resize_cached_terminals: true },
        progress: TerminalProgressPolicy::BackendSnapshot,
        persisted_sessions: PersistedSessionPolicy::AfterEmptyInitialSnapshot,
        generated_session_names: GeneratedSessionNamePolicy::PreserveBackend,
        terminal_residency: TerminalResidency::BindingScoped,
        selection_publication: SelectionPublicationPolicy::PersistBeforePublish,
    },
)]
#[case::native(
    MuxBackendKind::Native,
    MuxAppBackendPolicy {
        panes: PaneBehavior { topology: PaneTopology::ProcessLocal, cache_terminals: true, resize_cached_terminals: false },
        progress: TerminalProgressPolicy::TerminalOsc,
        persisted_sessions: PersistedSessionPolicy::Immediate,
        generated_session_names: GeneratedSessionNamePolicy::Reconcile,
        terminal_residency: TerminalResidency::WorkspaceShared,
        selection_publication: SelectionPublicationPolicy::Direct,
    },
)]
#[case::rmux(
    MuxBackendKind::Rmux,
    MuxAppBackendPolicy {
        panes: PaneBehavior { topology: PaneTopology::BackendReconciled, cache_terminals: true, resize_cached_terminals: false },
        progress: TerminalProgressPolicy::TerminalOsc,
        persisted_sessions: PersistedSessionPolicy::AfterEmptyInitialSnapshot,
        generated_session_names: GeneratedSessionNamePolicy::PreserveBackend,
        terminal_residency: TerminalResidency::BindingScoped,
        selection_publication: SelectionPublicationPolicy::PersistBeforePublish,
    },
)]
#[case::tmux(
    MuxBackendKind::Tmux,
    MuxAppBackendPolicy {
        panes: PaneBehavior { topology: PaneTopology::Attach, cache_terminals: true, resize_cached_terminals: true },
        progress: TerminalProgressPolicy::BackendSnapshot,
        persisted_sessions: PersistedSessionPolicy::Never,
        generated_session_names: GeneratedSessionNamePolicy::Reconcile,
        terminal_residency: TerminalResidency::BindingScoped,
        selection_publication: SelectionPublicationPolicy::Direct,
    },
)]
fn each_backend_owns_its_application_behavior_policy(
    #[case] backend: MuxBackendKind,
    #[case] expected: MuxAppBackendPolicy,
) {
    let registry = support::backends();
    let policy = registry.app_policy(&binding(backend));
    let expected = if cfg!(windows) && backend == MuxBackendKind::Tmux {
        MuxAppBackendPolicy {
            panes: PaneBehavior {
                topology: PaneTopology::ProcessLocal,
                cache_terminals: true,
                resize_cached_terminals: false,
            },
            progress: TerminalProgressPolicy::TerminalOsc,
            persisted_sessions: PersistedSessionPolicy::Immediate,
            generated_session_names: GeneratedSessionNamePolicy::Reconcile,
            terminal_residency: TerminalResidency::WorkspaceShared,
            selection_publication: SelectionPublicationPolicy::Direct,
        }
    } else {
        expected
    };

    assert_eq!(policy, expected);
}

#[test]
fn remote_tmux_keeps_attach_policy_on_every_host() {
    let registry = support::backends();
    let mut config = binding(MuxBackendKind::Tmux);
    config.remote = Some(SshTarget::for_host("example.test"));

    let pane_policy = registry.build_pane_policy(&config);
    let app_policy = registry.app_policy(&config);

    assert_eq!(
        pane_policy
            .remote_target()
            .map(|remote| remote.host.as_str()),
        Some("example.test")
    );
    assert_eq!(app_policy.panes.topology, PaneTopology::Attach);
}
