use bootty_mux::{
    MuxBackendKind, MuxBindingConfig, SshTarget,
    provider::{
        GeneratedSessionNamePolicy, PaneBehavior, PaneTopology, PersistedSessionPolicy,
        SelectionPublicationPolicy, TerminalProgressPolicy, TerminalResidency,
    },
};

mod support;

fn binding(backend: MuxBackendKind) -> MuxBindingConfig {
    MuxBindingConfig {
        backend,
        ..MuxBindingConfig::default()
    }
}

#[test]
fn each_backend_owns_its_application_behavior_policy() {
    let registry = support::backends();
    let cases = [
        (
            MuxBackendKind::Native,
            PaneBehavior {
                topology: PaneTopology::ProcessLocal,
                cache_terminals: true,
                resize_cached_terminals: false,
            },
            TerminalProgressPolicy::TerminalOsc,
            PersistedSessionPolicy::Immediate,
            GeneratedSessionNamePolicy::Reconcile,
            TerminalResidency::WorkspaceShared,
            SelectionPublicationPolicy::Direct,
        ),
        (
            MuxBackendKind::Rmux,
            PaneBehavior {
                topology: PaneTopology::BackendReconciled,
                cache_terminals: true,
                resize_cached_terminals: false,
            },
            TerminalProgressPolicy::TerminalOsc,
            PersistedSessionPolicy::AfterEmptyInitialSnapshot,
            GeneratedSessionNamePolicy::PreserveBackend,
            TerminalResidency::BindingScoped,
            SelectionPublicationPolicy::PersistBeforePublish,
        ),
        (
            MuxBackendKind::Tmux,
            PaneBehavior {
                topology: PaneTopology::Attach,
                cache_terminals: true,
                resize_cached_terminals: true,
            },
            TerminalProgressPolicy::BackendSnapshot,
            PersistedSessionPolicy::Never,
            GeneratedSessionNamePolicy::Reconcile,
            TerminalResidency::BindingScoped,
            SelectionPublicationPolicy::Direct,
        ),
        (
            MuxBackendKind::Zellij,
            PaneBehavior {
                topology: PaneTopology::Attach,
                cache_terminals: false,
                resize_cached_terminals: false,
            },
            TerminalProgressPolicy::TerminalOsc,
            PersistedSessionPolicy::Never,
            GeneratedSessionNamePolicy::Reconcile,
            TerminalResidency::BindingScoped,
            SelectionPublicationPolicy::Direct,
        ),
    ];

    for (
        backend,
        expected_panes,
        expected_progress,
        expected_sessions,
        expected_names,
        expected_residency,
        expected_selection,
    ) in cases
    {
        let policy = registry.app_policy(&binding(backend));
        let expected = if cfg!(windows) && backend == MuxBackendKind::Tmux {
            cases[0]
        } else {
            (
                backend,
                expected_panes,
                expected_progress,
                expected_sessions,
                expected_names,
                expected_residency,
                expected_selection,
            )
        };

        assert_eq!(policy.panes, expected.1);
        assert_eq!(policy.progress, expected.2);
        assert_eq!(policy.persisted_sessions, expected.3);
        assert_eq!(policy.generated_session_names, expected.4);
        assert_eq!(policy.terminal_residency, expected.5);
        assert_eq!(policy.selection_publication, expected.6);
    }
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
