use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::{Result, anyhow};
use bootty_mux::{
    MuxBackendKind, MuxBindingConfig,
    backend::MuxBackend,
    capability::{BindingCapabilityDescriptor, BindingOperation, BindingOperationOutcome},
    command::{MuxCommand, MuxSplitDirection},
    controller::{BindingId, MuxController, MuxScope, RepaintHandle, SpaceId},
    provider::{
        GeneratedSessionNamePolicy, MuxAppBackendPolicy, MuxAppBackendProvider,
        MuxAppBackendRegistry, MuxBackendProvider, MuxCommandDispatch, PaneBehavior, PaneTopology,
        PersistedSessionPolicy, SelectionPublicationPolicy, TerminalProgressPolicy,
        TerminalResidency,
    },
    snapshot::MuxSnapshot,
};

struct FakeBackend {
    execute_calls: Arc<AtomicUsize>,
}

impl MuxBackend for FakeBackend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        Ok(MuxSnapshot::default())
    }

    fn execute(&mut self, _command: MuxCommand) -> Result<()> {
        self.execute_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct FakeProvider {
    kind: MuxBackendKind,
    execute_calls: Arc<AtomicUsize>,
}

struct DynamicBackend {
    snapshot_calls: Arc<AtomicUsize>,
}

impl MuxBackend for DynamicBackend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        if self.snapshot_calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(MuxSnapshot::default())
        } else {
            Err(anyhow!("dynamic refresh failure"))
        }
    }

    fn execute(&mut self, _command: MuxCommand) -> Result<()> {
        Ok(())
    }
}

struct DynamicProvider {
    caller_thread: Arc<std::sync::atomic::AtomicBool>,
    snapshot_calls: Arc<AtomicUsize>,
}

impl MuxBackendProvider for DynamicProvider {
    fn kind(&self) -> MuxBackendKind {
        MuxBackendKind::Tmux
    }

    fn command_dispatch(&self) -> MuxCommandDispatch {
        if self.caller_thread.load(Ordering::SeqCst) {
            MuxCommandDispatch::CallerThread
        } else {
            MuxCommandDispatch::WorkerThread
        }
    }

    fn build_backend(
        &self,
        _config: &MuxBindingConfig,
        _workspace: Option<&Path>,
    ) -> Box<dyn MuxBackend> {
        Box::new(DynamicBackend {
            snapshot_calls: Arc::clone(&self.snapshot_calls),
        })
    }
}

impl MuxAppBackendProvider for DynamicProvider {
    fn app_policy(&self) -> MuxAppBackendPolicy {
        MuxAppBackendPolicy {
            panes: PaneBehavior {
                topology: PaneTopology::Attach,
                cache_terminals: false,
                resize_cached_terminals: false,
            },
            progress: TerminalProgressPolicy::BackendSnapshot,
            persisted_sessions: PersistedSessionPolicy::Never,
            generated_session_names: GeneratedSessionNamePolicy::Reconcile,
            terminal_residency: TerminalResidency::BindingScoped,
            selection_publication: SelectionPublicationPolicy::Direct,
        }
    }

    fn build_pane_policy(
        &self,
        _config: &MuxBindingConfig,
    ) -> Box<dyn bootty_mux::terminal::BackendPanePolicy> {
        unimplemented!()
    }

    fn capabilities(&self, scope: MuxScope) -> BindingCapabilityDescriptor {
        BindingCapabilityDescriptor::new(scope, [BindingOperation::SplitPane])
    }
}

impl MuxBackendProvider for FakeProvider {
    fn kind(&self) -> MuxBackendKind {
        self.kind
    }

    fn command_dispatch(&self) -> MuxCommandDispatch {
        MuxCommandDispatch::WorkerThread
    }

    fn build_backend(
        &self,
        _config: &MuxBindingConfig,
        _workspace: Option<&Path>,
    ) -> Box<dyn MuxBackend> {
        Box::new(FakeBackend {
            execute_calls: Arc::clone(&self.execute_calls),
        })
    }
}

impl MuxAppBackendProvider for FakeProvider {
    fn app_policy(&self) -> MuxAppBackendPolicy {
        MuxAppBackendPolicy {
            panes: PaneBehavior {
                topology: PaneTopology::Attach,
                cache_terminals: false,
                resize_cached_terminals: false,
            },
            progress: TerminalProgressPolicy::BackendSnapshot,
            persisted_sessions: PersistedSessionPolicy::Never,
            generated_session_names: GeneratedSessionNamePolicy::Reconcile,
            terminal_residency: TerminalResidency::BindingScoped,
            selection_publication: SelectionPublicationPolicy::Direct,
        }
    }

    fn build_pane_policy(
        &self,
        _config: &MuxBindingConfig,
    ) -> Box<dyn bootty_mux::terminal::BackendPanePolicy> {
        unimplemented!()
    }

    fn capabilities(&self, scope: MuxScope) -> BindingCapabilityDescriptor {
        BindingCapabilityDescriptor::new(scope, [BindingOperation::SplitPane])
    }
}

#[test]
fn unsupported_command_does_not_reach_backend() {
    let execute_calls = Arc::new(AtomicUsize::new(0));
    let core_provider: Arc<dyn MuxBackendProvider> = Arc::new(FakeProvider {
        kind: MuxBackendKind::Tmux,
        execute_calls: Arc::clone(&execute_calls),
    });
    let app_provider: Arc<dyn MuxAppBackendProvider> = Arc::new(FakeProvider {
        kind: MuxBackendKind::Tmux,
        execute_calls: Arc::clone(&execute_calls),
    });
    let registry = MuxAppBackendRegistry::from_providers(
        [core_provider],
        [app_provider],
        [MuxBackendKind::Tmux],
    )
    .unwrap();
    let config = MuxBindingConfig {
        backend: MuxBackendKind::Tmux,
        ..Default::default()
    };
    let scope = MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(1));
    let mut backend = FakeBackend {
        execute_calls: Arc::clone(&execute_calls),
    };
    let command = MuxCommand::SplitPane {
        session_id: "session".into(),
        pane_id: None,
        direction: MuxSplitDirection::Right,
    };
    let unsupported = MuxCommand::DitchSession {
        session_id: "session".into(),
    };

    assert!(matches!(
        registry.execute_checked(&config, scope, &mut backend, unsupported),
        BindingOperationOutcome::Unsupported
    ));
    assert_eq!(execute_calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        registry.execute_checked(&config, scope, &mut backend, command),
        BindingOperationOutcome::Supported(Ok(()))
    ));
    assert_eq!(execute_calls.load(Ordering::SeqCst), 1);
}

fn core_provider(kind: MuxBackendKind) -> Arc<dyn MuxBackendProvider> {
    Arc::new(FakeProvider {
        kind,
        execute_calls: Arc::new(AtomicUsize::new(0)),
    })
}

fn app_provider(kind: MuxBackendKind) -> Arc<dyn MuxAppBackendProvider> {
    Arc::new(FakeProvider {
        kind,
        execute_calls: Arc::new(AtomicUsize::new(0)),
    })
}

#[test]
fn app_registry_rejects_missing_duplicate_and_unmatched_app_providers() {
    assert!(
        MuxAppBackendRegistry::from_providers(
            [core_provider(MuxBackendKind::Tmux)],
            [],
            [MuxBackendKind::Tmux],
        )
        .err()
        .expect("missing app provider must fail")
        .to_string()
        .contains("missing app mux backend provider for Tmux")
    );
    assert!(
        MuxAppBackendRegistry::from_providers(
            [core_provider(MuxBackendKind::Tmux)],
            [
                app_provider(MuxBackendKind::Tmux),
                app_provider(MuxBackendKind::Tmux),
            ],
            [MuxBackendKind::Tmux],
        )
        .err()
        .expect("duplicate app provider must fail")
        .to_string()
        .contains("duplicate app mux backend provider for Tmux")
    );
    assert!(
        MuxAppBackendRegistry::from_providers(
            [core_provider(MuxBackendKind::Tmux)],
            [app_provider(MuxBackendKind::Zellij)],
            [MuxBackendKind::Tmux],
        )
        .err()
        .expect("unmatched app provider must fail")
        .to_string()
        .contains("app mux backend provider for Zellij has no core provider")
    );
}

#[test]
fn refresh_outcome_reports_one_applied_snapshot_without_latching_completion() {
    let registry = Arc::new(
        MuxAppBackendRegistry::from_providers(
            [core_provider(MuxBackendKind::Tmux)],
            [app_provider(MuxBackendKind::Tmux)],
            [MuxBackendKind::Tmux],
        )
        .unwrap(),
    );
    let config = MuxBindingConfig {
        backend: MuxBackendKind::Tmux,
        ..Default::default()
    };
    let scope = MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(1));
    let repaint: RepaintHandle = Arc::new(|| {});
    let mut controller = MuxController::new(scope, registry, None);

    let first = controller.refresh_sessions(&repaint, &config, Duration::from_secs(1));
    let applied = if first.applied {
        first
    } else {
        (0..100)
            .map(|_| {
                std::thread::sleep(Duration::from_millis(1));
                controller.refresh_sessions(&repaint, &config, Duration::from_secs(1))
            })
            .find(|outcome| outcome.applied)
            .expect("queued snapshot must eventually be applied")
    };

    assert!(applied.applied);
    assert_eq!(applied.error, None);
    let next = controller.refresh_sessions(&repaint, &config, Duration::from_secs(1));
    assert!(!next.applied);
    assert_eq!(next.error, None);
}

#[test]
fn refresh_outcome_keeps_applied_and_error_as_independent_fields() {
    let caller_thread = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let snapshot_calls = Arc::new(AtomicUsize::new(0));
    let registry = Arc::new(
        MuxAppBackendRegistry::from_providers(
            [Arc::new(DynamicProvider {
                caller_thread: Arc::clone(&caller_thread),
                snapshot_calls: Arc::clone(&snapshot_calls),
            }) as Arc<dyn MuxBackendProvider>],
            [Arc::new(DynamicProvider {
                caller_thread: Arc::clone(&caller_thread),
                snapshot_calls: Arc::clone(&snapshot_calls),
            }) as Arc<dyn MuxAppBackendProvider>],
            [MuxBackendKind::Tmux],
        )
        .unwrap(),
    );
    let config = MuxBindingConfig {
        backend: MuxBackendKind::Tmux,
        ..Default::default()
    };
    let scope = MuxScope::new(SpaceId::from_persistence(2), BindingId::from_persistence(2));
    let repaint: RepaintHandle = Arc::new(|| {});
    let mut controller = MuxController::new(scope, registry, None);

    let queued = controller.refresh_sessions(&repaint, &config, Duration::ZERO);
    assert!(!queued.applied);
    caller_thread.store(true, Ordering::SeqCst);
    let combined = (0..100)
        .map(|_| {
            std::thread::sleep(Duration::from_millis(1));
            controller.refresh_sessions(&repaint, &config, Duration::ZERO)
        })
        .find(|outcome| outcome.applied && outcome.error.is_some())
        .expect("one refresh must report the applied snapshot and later error");

    assert!(combined.applied);
    assert_eq!(combined.error.as_deref(), Some("dynamic refresh failure"));
    assert_eq!(controller.last_error(), Some("dynamic refresh failure"));
    assert_eq!(
        controller.unavailable_reason(),
        Some("dynamic refresh failure")
    );
}
