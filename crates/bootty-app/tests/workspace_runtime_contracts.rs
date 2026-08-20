use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use bootty_app::{
    AppState, FrameInputs, ModalDialog, ViewportSnapshot,
    config::{BoottyConfig, MultiplexerBackendConfig, SshProfileConfig},
    geometry::ViewTransform,
    mux::{
        MuxBackendKind, MuxBindingConfig, SshTarget,
        backend::MuxBackend,
        capability::{BindingCapabilityDescriptor, BindingOperation},
        command::MuxCommand,
        controller::MuxScope,
        provider::{
            GeneratedSessionNamePolicy, MuxAppBackendPolicy, MuxAppBackendProvider,
            MuxBackendProvider, MuxBackendRegistry, MuxCommandDispatch, PaneBehavior, PaneTopology,
            PersistedSessionPolicy, SelectionPublicationPolicy, TerminalProgressPolicy,
            TerminalResidency,
        },
        snapshot::{MuxPaneAnchor, MuxSession, MuxSnapshot, MuxWindow},
        terminal::{
            BackendPanePolicy, PaneLayoutResizeRequest, PaneStartRequest, ScopedMuxPaneTarget,
            TerminalRuntime,
        },
    },
    renderer::RendererMetrics,
    ui::{
        ditch::{DitchAction, DitchSessionEvent},
        new_session_picker::{NewMuxSessionDialog, NewSessionPickerEvent},
    },
};
use bootty_command::{
    AppCommandRequest, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
};
use bootty_workspace::{
    BindingMembershipMutation, RemoteSpaceRef, SpaceMuxOverride, SpaceRemoteOverride,
    WorkspaceRepository,
};
use rusqlite::Connection;

mod support;

struct RestoreBackend {
    sessions: Arc<Mutex<Vec<MuxSession>>>,
    create_calls: Arc<AtomicUsize>,
    release: Option<Arc<Mutex<mpsc::Receiver<()>>>>,
}

impl MuxBackend for RestoreBackend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        let sessions = self
            .sessions
            .lock()
            .expect("restore backend sessions lock")
            .clone();
        Ok(MuxSnapshot {
            active_session_id: sessions.first().map(|session| session.id.clone()),
            sessions,
            ..MuxSnapshot::default()
        })
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        match command {
            MuxCommand::CreateProjectSession { session_id, cwd } => {
                self.create_calls.fetch_add(1, Ordering::SeqCst);
                self.sessions
                    .lock()
                    .expect("restore backend sessions lock")
                    .push(MuxSession {
                        anchor: MuxPaneAnchor {
                            session_id: session_id.clone(),
                            cwd: Some(cwd),
                            ..MuxPaneAnchor::default()
                        },
                        id: session_id.clone(),
                        name: session_id,
                        active: true,
                        active_window_id: None,
                        windows: Vec::new(),
                    });
            }
            MuxCommand::DitchSession { session_id } => {
                if let Some(release) = &self.release {
                    release
                        .lock()
                        .expect("restore backend release lock")
                        .recv()
                        .expect("release delayed ditch");
                }
                self.sessions
                    .lock()
                    .expect("restore backend sessions lock")
                    .retain(|session| session.id != session_id);
            }
            _ => {}
        }
        Ok(())
    }
}

struct RestoreProvider {
    kind: MuxBackendKind,
    dispatch: MuxCommandDispatch,
    sessions: Arc<Mutex<Vec<MuxSession>>>,
    create_calls: Arc<AtomicUsize>,
    release: Option<Arc<Mutex<mpsc::Receiver<()>>>>,
    native_panes: bool,
    selection_publication: SelectionPublicationPolicy,
}

struct TestPanePolicy {
    fail_start: bool,
}

impl BackendPanePolicy for TestPanePolicy {
    fn remote_target(&self) -> Option<&SshTarget> {
        None
    }

    fn start_terminal(
        &mut self,
        _request: PaneStartRequest<'_>,
    ) -> Result<Option<Box<dyn TerminalRuntime>>> {
        if self.fail_start {
            anyhow::bail!("native pane publication failed")
        } else {
            Ok(None)
        }
    }

    fn sync_target(&mut self, _target: Option<&ScopedMuxPaneTarget>, _hide_tmux_status: bool) {}

    fn set_layout_window(&mut self, _window_id: Option<&str>) {}

    fn resize_layout_window(&mut self, _request: PaneLayoutResizeRequest<'_>) -> Result<bool> {
        Ok(false)
    }

    fn deactivate(&mut self) {}
}

impl MuxBackendProvider for RestoreProvider {
    fn kind(&self) -> MuxBackendKind {
        self.kind
    }

    fn command_dispatch(&self) -> MuxCommandDispatch {
        self.dispatch
    }

    fn build_backend(
        &self,
        _config: &MuxBindingConfig,
        _workspace: Option<&Path>,
    ) -> Box<dyn MuxBackend> {
        Box::new(RestoreBackend {
            sessions: Arc::clone(&self.sessions),
            create_calls: Arc::clone(&self.create_calls),
            release: self.release.clone(),
        })
    }
}

impl MuxAppBackendProvider for RestoreProvider {
    fn build_pane_policy(&self, _config: &MuxBindingConfig) -> Box<dyn BackendPanePolicy> {
        Box::new(TestPanePolicy {
            fail_start: self.native_panes,
        })
    }

    fn app_policy(&self) -> MuxAppBackendPolicy {
        MuxAppBackendPolicy {
            panes: PaneBehavior {
                topology: if self.native_panes {
                    PaneTopology::ProcessLocal
                } else {
                    PaneTopology::Attach
                },
                cache_terminals: false,
                resize_cached_terminals: false,
            },
            progress: TerminalProgressPolicy::BackendSnapshot,
            persisted_sessions: PersistedSessionPolicy::AfterEmptyInitialSnapshot,
            generated_session_names: GeneratedSessionNamePolicy::PreserveBackend,
            terminal_residency: TerminalResidency::BindingScoped,
            selection_publication: self.selection_publication,
        }
    }

    fn capabilities(&self, scope: MuxScope) -> BindingCapabilityDescriptor {
        BindingCapabilityDescriptor::new(
            scope,
            [
                BindingOperation::CreateProjectSession,
                BindingOperation::CreateWindow,
                BindingOperation::RenameSession,
                BindingOperation::DitchSession,
            ],
        )
    }
}

fn backends_after_empty_restore() -> (Arc<MuxBackendRegistry>, Arc<AtomicUsize>) {
    let sessions = Arc::new(Mutex::new(Vec::new()));
    let create_calls = Arc::new(AtomicUsize::new(0));
    let provider = || RestoreProvider {
        kind: MuxBackendKind::Tmux,
        dispatch: MuxCommandDispatch::CallerThread,
        sessions: Arc::clone(&sessions),
        create_calls: Arc::clone(&create_calls),
        release: None,
        native_panes: false,
        selection_publication: SelectionPublicationPolicy::Direct,
    };
    let registry = Arc::new(
        MuxBackendRegistry::from_app_providers([Arc::new(provider())], [MuxBackendKind::Tmux])
            .expect("restore test backend registry"),
    );
    (registry, create_calls)
}

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

fn session_with_pane(id: &str) -> MuxSession {
    let pane = MuxPaneAnchor {
        session_id: id.to_owned(),
        pane_id: Some(format!("{id}-pane")),
        ..MuxPaneAnchor::default()
    };
    MuxSession {
        id: id.to_owned(),
        name: id.to_owned(),
        active: id == "first",
        anchor: pane.clone(),
        active_window_id: Some(format!("{id}-window")),
        windows: vec![MuxWindow {
            id: format!("{id}-window"),
            index: 0,
            name: "window".to_owned(),
            active: true,
            anchor: pane.clone(),
            panes: vec![pane],
            layout: None,
            progress: None,
        }],
    }
}

fn submit_command(
    state: &mut AppState,
    invocation: CommandInvocation,
    started: Instant,
) -> CommandOutcome {
    let commands = state.app_command_sender(Caller::Socket);
    let (response, outcomes) = mpsc::channel();
    commands
        .try_send(AppCommandRequest {
            invocation,
            deadline: started + Duration::from_secs(1),
            cancellation: CommandCancellation::new(),
            response,
        })
        .expect("submit command");
    (0..250)
        .find_map(|tick| {
            state.update_frame(frame(started + Duration::from_millis(tick)));
            outcomes.try_recv().ok()
        })
        .expect("command completes")
}

#[test]
fn persist_before_publish_blocks_selection_when_restore_write_fails() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let config = BoottyConfig {
        config_path: config_path.clone(),
        multiplexer: bootty_app::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Tmux,
            ..bootty_app::config::MultiplexerConfig::default()
        },
        ..BoottyConfig::default()
    };
    let sessions = Arc::new(Mutex::new(vec![
        session_with_pane("first"),
        session_with_pane("second"),
    ]));
    let backends = Arc::new(
        MuxBackendRegistry::from_app_providers(
            [Arc::new(RestoreProvider {
                kind: MuxBackendKind::Tmux,
                dispatch: MuxCommandDispatch::CallerThread,
                sessions,
                create_calls: Arc::new(AtomicUsize::new(0)),
                release: None,
                native_panes: false,
                selection_publication: SelectionPublicationPolicy::PersistBeforePublish,
            })],
            [MuxBackendKind::Tmux],
        )
        .expect("selection test backend registry"),
    );
    let mut state =
        AppState::new(config, backends, Arc::new(|| {}), None, None).expect("app state");
    state.update_frame(frame(Instant::now()));
    assert_eq!(state.mux().selected_session(), Some("first"));

    let database = directory.path().join("session-order.sqlite3");
    let lock = Connection::open(&database).expect("open lock connection");
    lock.execute_batch("BEGIN IMMEDIATE")
        .expect("hold workspace write lock");
    state.activate_session_from_ui("second");

    assert_eq!(state.mux().selected_session(), Some("first"));
    assert!(
        state
            .last_error()
            .is_some_and(|error| error.contains("save binding restore state"))
    );
}

#[test]
fn native_pane_publication_error_is_preserved_on_successful_command() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let config = BoottyConfig {
        config_path,
        multiplexer: bootty_app::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Tmux,
            ..bootty_app::config::MultiplexerConfig::default()
        },
        ..BoottyConfig::default()
    };
    let backends = Arc::new(
        MuxBackendRegistry::from_app_providers(
            [Arc::new(RestoreProvider {
                kind: MuxBackendKind::Tmux,
                dispatch: MuxCommandDispatch::CallerThread,
                sessions: Arc::new(Mutex::new(vec![session_with_pane("first")])),
                create_calls: Arc::new(AtomicUsize::new(0)),
                release: None,
                native_panes: true,
                selection_publication: SelectionPublicationPolicy::Direct,
            })],
            [MuxBackendKind::Tmux],
        )
        .expect("native pane test backend registry"),
    );
    let mut state =
        AppState::new(config, backends, Arc::new(|| {}), None, None).expect("app state");
    state.update_frame(frame(Instant::now()));
    state.clear_last_error();

    let outcome = submit_command(
        &mut state,
        CommandInvocation::from_action("new_tab", Caller::Socket),
        Instant::now(),
    );

    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(state.last_error(), Some("native pane publication failed"));
}

#[test]
fn pending_ditch_completes_in_its_original_space() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let config = BoottyConfig {
        config_path: config_path.clone(),
        multiplexer: bootty_app::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Tmux,
            ..bootty_app::config::MultiplexerConfig::default()
        },
        ..BoottyConfig::default()
    };
    let cwd = directory.path().to_string_lossy().into_owned();
    let (release_tx, release_rx) = mpsc::channel();
    let release = Arc::new(Mutex::new(release_rx));
    let first_sessions = Arc::new(Mutex::new(vec![MuxSession {
        anchor: MuxPaneAnchor {
            session_id: "delayed".to_owned(),
            cwd: Some(cwd.clone()),
            ..MuxPaneAnchor::default()
        },
        id: "delayed".to_owned(),
        name: "delayed".to_owned(),
        active: true,
        active_window_id: None,
        windows: Vec::new(),
    }]));
    let second_sessions = Arc::new(Mutex::new(Vec::new()));
    let create_calls = Arc::new(AtomicUsize::new(0));
    let backends = Arc::new(
        MuxBackendRegistry::from_app_providers(
            [
                Arc::new(RestoreProvider {
                    kind: MuxBackendKind::Tmux,
                    dispatch: MuxCommandDispatch::WorkerThread,
                    sessions: Arc::clone(&first_sessions),
                    create_calls: Arc::clone(&create_calls),
                    release: Some(Arc::clone(&release)),
                    native_panes: false,
                    selection_publication: SelectionPublicationPolicy::Direct,
                }),
                Arc::new(RestoreProvider {
                    kind: MuxBackendKind::Zellij,
                    dispatch: MuxCommandDispatch::CallerThread,
                    sessions: Arc::clone(&second_sessions),
                    create_calls: Arc::clone(&create_calls),
                    release: None,
                    native_panes: false,
                    selection_publication: SelectionPublicationPolicy::Direct,
                }),
            ],
            [MuxBackendKind::Tmux, MuxBackendKind::Zellij],
        )
        .expect("delayed test backend registry"),
    );

    let (mut repository, snapshot) = WorkspaceRepository::open(&config_path).expect("workspace");
    let first_space = snapshot.spaces()[0].clone();
    let first_scope = first_space.bindings()[0].mux_scope();
    let mut order = first_space.bindings()[0].session_order().clone();
    assert!(order.add_session("delayed"));
    let mut names = first_space.bindings()[0].session_names().clone();
    assert!(names.remember_generated("delayed", &cwd, "delayed", "delayed"));
    repository
        .commit_binding_state(first_scope, &order, &names)
        .expect("persist delayed session");
    let second_space = repository
        .create_space(
            "Second",
            "2",
            [0x22, 0x44, 0x66],
            false,
            SpaceMuxOverride {
                backend: Some(MultiplexerBackendConfig::Zellij),
                remote: SpaceRemoteOverride::Local,
            },
            config.multiplexer.hide_tmux_status,
        )
        .expect("create second Space")
        .expect("valid second Space");
    let second_id = second_space.id();
    let second_scope = second_space.bindings()[0].mux_scope();
    drop(repository);

    let mut state =
        AppState::new(config, backends, Arc::new(|| {}), None, None).expect("app state");
    assert!((0..250).any(|tick| {
        state.update_frame(frame(Instant::now() + Duration::from_millis(tick)));
        std::thread::sleep(Duration::from_millis(1));
        state
            .binding_session_groups()
            .iter()
            .flat_map(|group| group.sessions.iter())
            .any(|session| session.id == "delayed")
    }));
    assert!(state.open_ditch_session_dialog_for("delayed"));
    let ModalDialog::DitchSession(dialog) = state.take_modal_dialog().expect("ditch dialog") else {
        panic!("expected ditch dialog");
    };
    state.clear_last_error();
    state.apply_ditch_session_event(
        dialog,
        DitchSessionEvent::Ditch {
            session_id: "delayed".to_owned(),
            cwd: None,
            action: DitchAction::KillOnly,
        },
    );
    assert!(state.activate_space_from_ui(second_id));
    assert!(state.binding_session_groups()[0].sessions.is_empty());
    release_tx.send(()).expect("release delayed ditch");

    for tick in 0..250 {
        state.update_frame(frame(Instant::now() + Duration::from_millis(tick)));
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(state.last_error().is_none(), "{:#?}", state.last_error());
    assert!(state.activate_space_from_ui(first_scope.space_id()));
    let removed = (0..250).any(|tick| {
        state.update_frame(frame(Instant::now() + Duration::from_millis(tick)));
        std::thread::sleep(Duration::from_millis(1));
        state.binding_session_groups()[0].sessions.is_empty()
    });
    assert!(removed, "original Space must publish the ditch completion");

    drop(state);
    let (_, reopened) = WorkspaceRepository::open(&config_path).expect("reopen workspace");
    let first = reopened
        .spaces()
        .iter()
        .find(|space| space.id() == first_scope.space_id())
        .expect("first Space");
    let second = reopened
        .spaces()
        .iter()
        .find(|space| space.id() == second_scope.space_id())
        .expect("second Space");
    assert!(
        first.bindings()[0]
            .session_order()
            .session_names()
            .is_empty()
    );
    assert!(
        second.bindings()[0]
            .session_order()
            .session_names()
            .is_empty()
    );
}

#[test]
fn a_failed_placement_commit_preserves_the_live_and_durable_binding() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let config = BoottyConfig {
        config_path: config_path.clone(),
        multiplexer: bootty_app::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Native,
            ..bootty_app::config::MultiplexerConfig::default()
        },
        ..BoottyConfig::default()
    };
    let repaint = Arc::new(|| {});
    let mut state =
        AppState::new(config, support::backends(), repaint, None, None).expect("app state");
    let space = state.space_summaries()[0].clone();
    let database = directory.path().join("session-order.sqlite3");
    let lock = Connection::open(&database).expect("open lock connection");
    lock.execute_batch("BEGIN IMMEDIATE")
        .expect("hold workspace write lock");

    assert!(!state.update_space_from_ui(
        space.id,
        &space.name,
        &space.icon,
        space.color,
        space.tint_sidebar,
        SpaceMuxOverride {
            backend: Some(MultiplexerBackendConfig::Tmux),
            remote: SpaceRemoteOverride::Local,
        },
    ));
    assert_eq!(
        state.multiplexer_backend(),
        MultiplexerBackendConfig::Native
    );

    lock.execute_batch("ROLLBACK").expect("release write lock");
    drop(lock);
    let (_, reopened) = WorkspaceRepository::open(&config_path).expect("reopen workspace");
    let binding = &reopened.spaces()[0].bindings()[0];
    assert_eq!(binding.backend_override(), None);
    assert_eq!(binding.remote_override(), &SpaceRemoteOverride::Inherit);
}

#[test]
fn a_failed_session_membership_commit_preserves_the_live_runtime_and_database() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let mut config = BoottyConfig {
        config_path: config_path.clone(),
        ..BoottyConfig::default()
    };
    config.multiplexer.backend = MultiplexerBackendConfig::Native;
    let repaint = Arc::new(|| {});
    let mut state =
        AppState::new(config, support::backends(), repaint, None, None).expect("app state");
    let commands = state.app_command_sender(Caller::Socket);
    let (response, outcomes) = mpsc::channel();
    commands
        .try_send(AppCommandRequest {
            invocation: CommandInvocation::from_action("new_tab", Caller::Socket),
            // The budget bounds a genuine hang. It stays far above the scheduler jitter that a
            // fully parallel test run adds to a pane spawn.
            deadline: Instant::now() + Duration::from_secs(30),
            cancellation: CommandCancellation::new(),
            response,
        })
        .expect("submit command");

    let started = Instant::now();
    let outcome = (0..250)
        .find_map(|tick| {
            state.update_frame(frame(started + Duration::from_millis(tick)));
            std::thread::sleep(Duration::from_millis(1));
            outcomes.try_recv().ok()
        })
        .expect("create session command completes");
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "unexpected command outcome: {outcome:?}"
    );
    let target = (0..250)
        .find_map(|tick| {
            state.update_frame(frame(started + Duration::from_millis(250 + tick)));
            std::thread::sleep(Duration::from_millis(1));
            state
                .binding_session_groups()
                .into_iter()
                .find_map(|group| group.sessions.first().map(|session| group.target(session)))
        })
        .expect("native session becomes available");
    let original_name = state
        .binding_session_groups()
        .iter()
        .flat_map(|group| group.sessions.iter())
        .find(|session| session.id == target.session_id)
        .expect("live session")
        .name
        .clone();

    let database = directory.path().join("session-order.sqlite3");
    let lock = Connection::open(&database).expect("open lock connection");
    lock.execute_batch("BEGIN IMMEDIATE")
        .expect("hold workspace write lock");

    assert!(!state.detach_scoped_session_from_space(&target));
    assert!(
        state
            .binding_session_groups()
            .iter()
            .flat_map(|group| group.sessions.iter())
            .any(|session| session.id == target.session_id)
    );

    lock.execute_batch("ROLLBACK").expect("release write lock");
    drop(lock);
    let (_, reopened) = WorkspaceRepository::open(&config_path).expect("reopen workspace");
    assert!(
        reopened.spaces()[0].bindings()[0]
            .session_order()
            .session_names()
            .contains(&original_name)
    );
}

#[test]
fn an_inactive_placement_update_rebuilds_before_activation() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let config = BoottyConfig {
        config_path: config_path.clone(),
        multiplexer: bootty_app::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Native,
            ..bootty_app::config::MultiplexerConfig::default()
        },
        ..BoottyConfig::default()
    };
    let (mut repository, _) = WorkspaceRepository::open(&config_path).expect("workspace");
    let second_space = repository
        .create_space(
            "Second",
            "2",
            [0x22, 0x44, 0x66],
            false,
            SpaceMuxOverride::default(),
            config.multiplexer.hide_tmux_status,
        )
        .expect("create second Space")
        .expect("valid second Space");
    let second_id = second_space.id();
    drop(repository);

    let repaint = Arc::new(|| {});
    let mut state =
        AppState::new(config, support::backends(), repaint, None, None).expect("app state");
    let second = state
        .space_summaries()
        .into_iter()
        .find(|space| space.id == second_id)
        .expect("inactive second Space");
    assert!(state.update_space_from_ui(
        second.id,
        &second.name,
        &second.icon,
        second.color,
        second.tint_sidebar,
        SpaceMuxOverride {
            backend: Some(MultiplexerBackendConfig::Tmux),
            remote: SpaceRemoteOverride::Local,
        },
    ));
    assert_eq!(
        state.multiplexer_backend(),
        MultiplexerBackendConfig::Native
    );
    assert!(state.activate_space_from_ui(second_id));
    assert_eq!(state.multiplexer_backend(), MultiplexerBackendConfig::Tmux);
}

#[test]
fn deleting_an_inactive_space_removes_live_and_durable_state() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let config = BoottyConfig {
        config_path: config_path.clone(),
        ..BoottyConfig::default()
    };
    let (mut repository, _) = WorkspaceRepository::open(&config_path).expect("workspace");
    let second_space = repository
        .create_space(
            "Second",
            "2",
            [0x22, 0x44, 0x66],
            false,
            SpaceMuxOverride::default(),
            config.multiplexer.hide_tmux_status,
        )
        .expect("create second Space")
        .expect("valid second Space");
    let second_id = second_space.id();
    drop(repository);

    let repaint = Arc::new(|| {});
    let mut state =
        AppState::new(config.clone(), support::backends(), repaint, None, None).expect("app state");
    assert_eq!(state.space_summaries().len(), 2);
    assert!(state.close_space_from_ui(second_id));
    assert!(
        state
            .space_summaries()
            .into_iter()
            .all(|space| space.id != second_id)
    );

    let (_, snapshot) = WorkspaceRepository::open(&config_path).expect("reopen workspace");
    assert_eq!(snapshot.spaces().len(), 1);
    assert!(
        snapshot
            .spaces()
            .iter()
            .all(|space| space.id() != second_id)
    );
}

#[test]
fn one_frame_recovers_active_and_inactive_binding_membership() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let config = BoottyConfig {
        config_path: config_path.clone(),
        multiplexer: bootty_app::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Tmux,
            ..bootty_app::config::MultiplexerConfig::default()
        },
        ..BoottyConfig::default()
    };
    let (mut repository, snapshot) =
        WorkspaceRepository::open(&config_path).expect("workspace repository");
    let first_scope = snapshot.spaces()[0].bindings()[0].mux_scope();
    let first_binding = snapshot.spaces()[0].bindings()[0].clone();
    let mut session_order = first_binding.session_order().clone();
    assert!(session_order.add_session("persisted-first"));
    let mut session_names = first_binding.session_names().clone();
    assert!(session_names.remember_generated(
        "persisted-first",
        directory.path().to_str().expect("workspace path"),
        "persisted-first",
        "persisted-first",
    ));
    repository
        .commit_binding_state(first_scope, &session_order, &session_names)
        .expect("persist first binding state");
    let second_space = repository
        .create_space(
            "Second",
            "2",
            [0x22, 0x44, 0x66],
            false,
            SpaceMuxOverride::default(),
            config.multiplexer.hide_tmux_status,
        )
        .expect("create second Space")
        .expect("valid second Space");
    let second_scope = second_space.bindings()[0].mux_scope();
    for (scope, name) in [
        (first_scope, "interrupted-first"),
        (second_scope, "interrupted-second"),
    ] {
        repository
            .begin_binding_membership_mutation(
                scope,
                &BindingMembershipMutation::Create {
                    session_id: name.to_owned(),
                    session_name: name.to_owned(),
                    display_name: name.to_owned(),
                    explicit: true,
                    cwd: None,
                },
            )
            .expect("journal interrupted membership operation");
    }

    let repaint = Arc::new(|| {});
    let (backends, create_calls) = backends_after_empty_restore();
    let mut state = AppState::new(config, backends, repaint, None, None).expect("app state");
    assert_eq!(create_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(
        state
            .binding_session_groups()
            .iter()
            .flat_map(|group| &group.sessions)
            .all(|session| session.name != "persisted-first")
    );
    let spaces = state.space_summaries();
    let first_space = spaces
        .iter()
        .find(|space| space.id == first_scope.space_id())
        .expect("first Space")
        .clone();
    let second_space = spaces
        .iter()
        .find(|space| space.id == second_scope.space_id())
        .expect("second Space")
        .clone();
    let local_override = SpaceMuxOverride {
        backend: None,
        remote: SpaceRemoteOverride::Local,
    };
    for space in [&first_space, &second_space] {
        assert!(!state.update_space_from_ui(
            space.id,
            &space.name,
            &space.icon,
            space.color,
            space.tint_sidebar,
            local_override.clone(),
        ));
    }
    assert!(
        state
            .last_error()
            .is_some_and(|error| error.contains("pending binding membership recovery"))
    );

    state.update_frame(frame(Instant::now()));
    assert!(
        repository
            .pending_binding_membership_mutation(first_scope)
            .expect("read first pending operation")
            .is_none(),
        "one active frame must resolve the first journal"
    );
    assert!(
        repository
            .pending_binding_membership_mutation(second_scope)
            .expect("read second pending operation")
            .is_none(),
        "one frame must resolve the inactive binding journal"
    );
    let active_groups = state.binding_session_groups();
    assert_eq!(active_groups.len(), 1);
    assert_eq!(active_groups[0].scope, first_scope);
    assert!(active_groups[0].active);
    assert_eq!(create_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(state.update_space_from_ui(
        first_space.id,
        &first_space.name,
        &first_space.icon,
        first_space.color,
        first_space.tint_sidebar,
        local_override.clone(),
    ));
    assert!(state.update_space_from_ui(
        second_space.id,
        &second_space.name,
        &second_space.icon,
        second_space.color,
        second_space.tint_sidebar,
        local_override.clone(),
    ));

    assert!(state.activate_space_from_ui(second_space.id));
    let active_groups = state.binding_session_groups();
    assert_eq!(active_groups.len(), 1);
    assert_eq!(active_groups[0].scope, second_scope);
    assert!(active_groups[0].active);
    state.update_frame(frame(Instant::now()));
    assert!(
        repository
            .pending_binding_membership_mutation(second_scope)
            .expect("read second pending operation")
            .is_none(),
        "one frame after activation must resolve the second journal"
    );
    assert!(state.update_space_from_ui(
        second_space.id,
        &second_space.name,
        &second_space.icon,
        second_space.color,
        second_space.tint_sidebar,
        local_override,
    ));

    let (_, reopened) = WorkspaceRepository::open(&config_path).expect("reopen workspace");
    assert!(
        reopened
            .spaces()
            .iter()
            .all(|space| { space.bindings()[0].remote_override() == &SpaceRemoteOverride::Local })
    );
}

#[test]
fn a_deferred_profile_rebuild_preserves_the_intended_display_name() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let mut config = BoottyConfig {
        config_path: config_path.clone(),
        multiplexer: bootty_app::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Native,
            ..bootty_app::config::MultiplexerConfig::default()
        },
        ..BoottyConfig::default()
    };
    config.ssh_profiles.insert(
        "test".to_owned(),
        SshProfileConfig {
            name: "Initial".to_owned(),
            host: "localhost".to_owned(),
            user: None,
            port: None,
            authentication: Default::default(),
            host_key_policy: Default::default(),
            identity_file: None,
            proxy_jump: None,
            program: "ssh".to_owned(),
            args: Vec::new(),
        },
    );
    let (mut repository, snapshot) =
        WorkspaceRepository::open(&config_path).expect("workspace repository");
    let space = &snapshot.spaces()[0];
    let scope = space.bindings()[0].mux_scope();
    repository
        .update_space_and_binding(
            scope,
            space.name(),
            space.icon(),
            space.color(),
            space.tint_sidebar(),
            SpaceMuxOverride {
                backend: Some(MultiplexerBackendConfig::Native),
                remote: SpaceRemoteOverride::Profile(RemoteSpaceRef {
                    profile_id: "test".to_owned(),
                    remote_space_id: "test-space".to_owned(),
                    remote_space_name: "Test Space".to_owned(),
                    backend: MultiplexerBackendConfig::Native,
                }),
            },
        )
        .expect("configure profile binding");

    let first_cwd = directory.path().join("first/project");
    let second_cwd = directory.path().join("second/project");
    std::fs::create_dir_all(&first_cwd).expect("first project directory");
    std::fs::create_dir_all(&second_cwd).expect("second project directory");
    let repaint = Arc::new(|| {});
    let mut state =
        AppState::new(config, support::backends(), repaint, None, None).expect("app state");
    state.apply_picker_event(
        NewMuxSessionDialog::open(),
        NewSessionPickerEvent::CreateSession {
            cwd: first_cwd.to_string_lossy().into_owned(),
        },
    );
    let started = Instant::now();
    assert!((0..250).any(|tick| {
        state.update_frame(frame(started + Duration::from_millis(tick)));
        std::thread::sleep(Duration::from_millis(1));
        state
            .binding_session_groups()
            .iter()
            .flat_map(|group| &group.sessions)
            .any(|session| session.name == "project")
    }));

    state.apply_picker_event(
        NewMuxSessionDialog::open(),
        NewSessionPickerEvent::CreateSession {
            cwd: second_cwd.to_string_lossy().into_owned(),
        },
    );
    std::fs::write(
        &config_path,
        "[ssh-profiles.test]\nname = \"Changed\"\nhost = \"localhost\"\nprogram = \"ssh\"\n",
    )
    .expect("write changed profile");
    assert!(state.reload_config(&mut Vec::new()));
    assert!(
        repository
            .pending_binding_membership_mutation(scope)
            .expect("read pending operation")
            .is_some(),
        "profile reload must defer while the membership command is pending"
    );

    assert!((0..250).any(|tick| {
        state.update_frame(frame(started + Duration::from_millis(250 + tick)));
        std::thread::sleep(Duration::from_millis(1));
        repository
            .pending_binding_membership_mutation(scope)
            .expect("read pending operation")
            .is_none()
    }));
    let (_, reopened) = WorkspaceRepository::open(&config_path).expect("reopen workspace");
    let names = reopened.spaces()[0].bindings()[0].session_names();
    let record = names
        .record("project-2")
        .expect("second session name metadata");
    assert_eq!(record.session_name, "project-2");
    assert_eq!(record.display_name, "project");
}

#[test]
fn a_corrected_ssh_profile_rebuilds_an_unavailable_binding() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let config = BoottyConfig {
        config_path: config_path.clone(),
        ..BoottyConfig::default()
    };
    let (mut repository, snapshot) =
        WorkspaceRepository::open(&config_path).expect("workspace repository");
    let space = &snapshot.spaces()[0];
    let scope = space.bindings()[0].mux_scope();
    repository
        .update_space_and_binding(
            scope,
            space.name(),
            space.icon(),
            space.color(),
            space.tint_sidebar(),
            SpaceMuxOverride {
                backend: Some(MultiplexerBackendConfig::Native),
                remote: SpaceRemoteOverride::Profile(RemoteSpaceRef {
                    profile_id: "development".to_owned(),
                    remote_space_id: "remote-space".to_owned(),
                    remote_space_name: "Remote Space".to_owned(),
                    backend: MultiplexerBackendConfig::Native,
                }),
            },
        )
        .expect("configure missing profile binding");
    let binding = space.bindings()[0].clone();
    let mut session_order = binding.session_order().clone();
    assert!(session_order.add_session("persisted-local-fallback"));
    let mut session_names = binding.session_names().clone();
    assert!(session_names.remember_generated(
        "persisted-local-fallback",
        directory.path().to_str().expect("workspace path"),
        "persisted-local-fallback",
        "persisted-local-fallback",
    ));
    repository
        .commit_binding_state(scope, &session_order, &session_names)
        .expect("persist unavailable binding restore state");

    let repaint = Arc::new(|| {});
    let mut state =
        AppState::new(config, support::backends(), repaint, None, None).expect("app state");
    assert_eq!(
        state.space_summaries()[0].error.as_deref(),
        Some("SSH profile 'development' is unavailable")
    );
    assert!(
        state
            .binding_session_groups()
            .iter()
            .flat_map(|group| &group.sessions)
            .all(|session| session.name != "persisted-local-fallback")
    );
    state.update_frame(frame(Instant::now()));
    assert_eq!(
        state.space_summaries()[0].error.as_deref(),
        Some("SSH profile 'development' is unavailable")
    );
    assert!(
        state
            .binding_session_groups()
            .iter()
            .flat_map(|group| &group.sessions)
            .all(|session| session.name != "persisted-local-fallback")
    );

    std::fs::write(
        &config_path,
        r#"
[ssh-profiles.development]
name = "Development"
host = "devbox"
user = "dev"
port = 2222
program = "ssh-wrapper"
args = ["-i", "key"]
"#,
    )
    .expect("write corrected profile");

    assert!(state.reload_config(&mut Vec::new()));
    assert_eq!(state.space_summaries()[0].error, None);
}
