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
    renderer::RendererMetrics,
    ui::{
        ditch::{DitchAction, DitchSessionEvent},
        new_session_picker::NewSessionPickerEvent,
    },
};
use bootty_command::{
    AppCommandRequest, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
};
use bootty_config::config::{BoottyConfig, MultiplexerBackendConfig, SshProfileConfig};
use bootty_mux::{
    MuxBackendKind, MuxBindingConfig, SshTarget,
    backend::MuxBackend,
    capability::{BindingCapabilityDescriptor, BindingOperation},
    command::MuxCommand,
    controller::SpaceId,
    provider::{
        GeneratedSessionNamePolicy, MuxAppBackendPolicy, MuxAppBackendProvider, MuxBackendProvider,
        MuxBackendRegistry, MuxCommandDispatch, PaneBehavior, PaneTopology, PersistedSessionPolicy,
        SelectionPublicationPolicy, TerminalProgressPolicy, TerminalResidency,
    },
    snapshot::{MuxPaneAnchor, MuxSession, MuxSessionTag, MuxSnapshot, MuxWindow},
    terminal::{
        BackendPanePolicy, PaneLayoutResizeRequest, PaneStartRequest, ScopedMuxPaneTarget,
        TerminalRuntime,
    },
};
use bootty_render::geometry::ViewTransform;
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
            MuxCommand::CreateProjectSession {
                session_id,
                cwd,
                tag,
            } => {
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
                        tag,
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
            MuxCommand::StampSession { session_id, tag } => {
                if let Some(session) = self
                    .sessions
                    .lock()
                    .expect("restore backend sessions lock")
                    .iter_mut()
                    .find(|session| session.id == session_id)
                {
                    session.tag = tag;
                }
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

    fn capabilities(&self, scope: SpaceId) -> BindingCapabilityDescriptor {
        BindingCapabilityDescriptor::new(
            scope,
            [
                BindingOperation::CreateProjectSession,
                BindingOperation::CreateWindow,
                BindingOperation::RenameSession,
                BindingOperation::DitchSession,
                BindingOperation::StampSession,
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

fn claimed_session(
    identity: &str,
    backend_name: &str,
    cwd: &str,
) -> bootty_workspace::WorkspaceSession {
    bootty_workspace::WorkspaceSession {
        identity: identity.to_owned(),
        backend_name: backend_name.to_owned(),
        display_name: String::new(),
        explicit: false,
        cwd: cwd.to_owned(),
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
        tag: MuxSessionTag::default(),
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
        multiplexer: bootty_config::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Tmux,
            ..bootty_config::config::MultiplexerConfig::default()
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
        multiplexer: bootty_config::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Tmux,
            ..bootty_config::config::MultiplexerConfig::default()
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

/// Handing a session to another Space is a change of claim, not of session: it keeps its identity,
/// its name, and the process it was running.
#[test]
fn a_session_moves_between_spaces_on_one_multiplexer_and_stays_put_across_a_restart() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let config = BoottyConfig {
        config_path: config_path.clone(),
        multiplexer: bootty_config::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Tmux,
            ..bootty_config::config::MultiplexerConfig::default()
        },
        ..BoottyConfig::default()
    };
    let cwd = directory.path().to_string_lossy().into_owned();

    let (mut repository, snapshot) = WorkspaceRepository::open(&config_path).expect("workspace");
    let first_space = snapshot.spaces()[0].clone();
    let first_scope = first_space.binding().mux_scope();
    let mut claimed = first_space.binding().sessions().clone();
    assert!(claimed.claim(claimed_session("moving-id", "moving", &cwd)));
    repository
        .commit_binding_state(first_scope, &claimed)
        .expect("persist the session");
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
    let second_scope = second_space.binding().mux_scope();
    // A Space on another multiplexer: a session cannot follow it there.
    let elsewhere = repository
        .create_space(
            "Elsewhere",
            "3",
            [0x33, 0x55, 0x77],
            false,
            SpaceMuxOverride {
                backend: Some(MultiplexerBackendConfig::Rmux),
                remote: SpaceRemoteOverride::Local,
            },
            config.multiplexer.hide_tmux_status,
        )
        .expect("create the far Space")
        .expect("valid far Space");
    let elsewhere_id = elsewhere.id();
    drop(repository);

    let sessions = Arc::new(Mutex::new(vec![MuxSession {
        anchor: MuxPaneAnchor {
            session_id: "moving".to_owned(),
            cwd: Some(cwd.clone()),
            ..MuxPaneAnchor::default()
        },
        id: "moving".to_owned(),
        name: "moving".to_owned(),
        active: true,
        active_window_id: None,
        tag: MuxSessionTag {
            identity: Some("moving-id".to_owned()),
            space: Some(first_space.remote_id().to_owned()),
        },
        windows: Vec::new(),
    }]));
    let create_calls = Arc::new(AtomicUsize::new(0));
    let provider = |kind| {
        Arc::new(RestoreProvider {
            kind,
            dispatch: MuxCommandDispatch::CallerThread,
            sessions: Arc::clone(&sessions),
            create_calls: Arc::clone(&create_calls),
            release: None,
            native_panes: false,
            selection_publication: SelectionPublicationPolicy::Direct,
        })
    };
    let backends = Arc::new(
        MuxBackendRegistry::from_app_providers(
            [
                provider(MuxBackendKind::Tmux),
                provider(MuxBackendKind::Rmux),
            ],
            [MuxBackendKind::Tmux, MuxBackendKind::Rmux],
        )
        .expect("move test registry"),
    );

    let mut state =
        AppState::new(config, backends, Arc::new(|| {}), None, None).expect("app state");
    state.update_frame(frame(Instant::now()));
    let target = bootty_app::ui::session_navigation::ScopedSessionTarget::new(
        first_scope,
        "moving".to_owned(),
    );

    // Both Spaces run the same multiplexer, so the move is a change of tag.
    let targets = state.session_move_targets(&target);
    assert!(
        targets
            .iter()
            .any(|space| space.id == second_id && space.reachable)
    );
    assert!(
        targets
            .iter()
            .any(|space| space.id == first_scope && space.current)
    );
    // Listed, so the answer is "not from here" rather than a Space that seems not to exist.
    assert!(
        targets
            .iter()
            .any(|space| space.id == elsewhere_id && !space.reachable),
        "a Space on another multiplexer is offered and refused, not hidden"
    );
    assert!(!state.move_scoped_session_to_space(&target, elsewhere_id));

    assert!(state.move_scoped_session_to_space(&target, second_id));
    assert!(
        !state.move_scoped_session_to_space(&target, first_scope),
        "the session no longer belongs to the Space it came from"
    );

    drop(state);
    let (_, reopened) = WorkspaceRepository::open(&config_path).expect("reopen workspace");
    let claims = |space_id| {
        reopened
            .spaces()
            .iter()
            .find(|space| space.id() == space_id)
            .expect("Space")
            .binding()
            .sessions()
            .backend_names()
    };
    assert!(claims(first_scope).is_empty());
    assert_eq!(claims(second_scope), vec!["moving"]);
    assert_eq!(
        sessions.lock().expect("sessions")[0].tag.space.as_deref(),
        Some(second_space.remote_id()),
        "the multiplexer carries the new claim, so every bootty window agrees"
    );
}

/// Letting go of a session must not make it disappear. Membership is explicit, so a session no
/// Space claims has to be somewhere the sidebar can show it.
#[test]
fn an_unassigned_session_keeps_running_and_stays_visible_as_unclaimed() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let config = BoottyConfig {
        config_path: config_path.clone(),
        multiplexer: bootty_config::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Tmux,
            ..bootty_config::config::MultiplexerConfig::default()
        },
        ..BoottyConfig::default()
    };
    let cwd = directory.path().to_string_lossy().into_owned();

    let (mut repository, snapshot) = WorkspaceRepository::open(&config_path).expect("workspace");
    let space = snapshot.spaces()[0].clone();
    let scope = space.binding().mux_scope();
    let mut claimed = space.binding().sessions().clone();
    assert!(claimed.claim(claimed_session("kept-id", "kept", &cwd)));
    repository
        .commit_binding_state(scope, &claimed)
        .expect("persist the session");
    drop(repository);

    let sessions = Arc::new(Mutex::new(vec![MuxSession {
        anchor: MuxPaneAnchor {
            session_id: "kept".to_owned(),
            cwd: Some(cwd.clone()),
            ..MuxPaneAnchor::default()
        },
        id: "kept".to_owned(),
        name: "kept".to_owned(),
        active: true,
        active_window_id: None,
        tag: MuxSessionTag {
            identity: Some("kept-id".to_owned()),
            space: Some(space.remote_id().to_owned()),
        },
        windows: Vec::new(),
    }]));
    let backends = Arc::new(
        MuxBackendRegistry::from_app_providers(
            [Arc::new(RestoreProvider {
                kind: MuxBackendKind::Tmux,
                dispatch: MuxCommandDispatch::CallerThread,
                sessions: Arc::clone(&sessions),
                create_calls: Arc::new(AtomicUsize::new(0)),
                release: None,
                native_panes: false,
                selection_publication: SelectionPublicationPolicy::Direct,
            })],
            [MuxBackendKind::Tmux],
        )
        .expect("unassign test registry"),
    );

    let mut state =
        AppState::new(config, backends, Arc::new(|| {}), None, None).expect("app state");
    state.update_frame(frame(Instant::now()));
    let target =
        bootty_app::ui::session_navigation::ScopedSessionTarget::new(scope, "kept".to_owned());
    assert!(state.unclaimed_sessions().is_empty());

    assert!(state.detach_scoped_session_from_space(&target));
    state.update_frame(frame(Instant::now()));

    assert_eq!(
        sessions.lock().expect("sessions")[0].tag.space,
        None,
        "the session is running and claimed by nobody"
    );
    assert_eq!(
        sessions.lock().expect("sessions")[0]
            .tag
            .identity
            .as_deref(),
        Some("kept-id"),
        "its identity survives, so a Space can take it back without minting a new one"
    );
    let unclaimed = state.unclaimed_sessions();
    assert_eq!(
        unclaimed
            .iter()
            .map(|session| session.name.as_str())
            .collect::<Vec<_>>(),
        ["kept"],
        "an unassigned session is visible, not gone"
    );

    // And taking it back is one click.
    assert!(state.adopt_and_activate_scoped_session(&target));
    state.update_frame(frame(Instant::now()));
    assert!(state.unclaimed_sessions().is_empty());
}

#[test]
fn pending_ditch_completes_in_its_original_space() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let config = BoottyConfig {
        config_path: config_path.clone(),
        multiplexer: bootty_config::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Tmux,
            ..bootty_config::config::MultiplexerConfig::default()
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
        tag: MuxSessionTag::default(),
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
                    kind: MuxBackendKind::Rmux,
                    dispatch: MuxCommandDispatch::CallerThread,
                    sessions: Arc::clone(&second_sessions),
                    create_calls: Arc::clone(&create_calls),
                    release: None,
                    native_panes: false,
                    selection_publication: SelectionPublicationPolicy::Direct,
                }),
            ],
            [MuxBackendKind::Tmux, MuxBackendKind::Rmux],
        )
        .expect("delayed test backend registry"),
    );

    let (mut repository, snapshot) = WorkspaceRepository::open(&config_path).expect("workspace");
    let first_space = snapshot.spaces()[0].clone();
    let first_scope = first_space.binding().mux_scope();
    let mut sessions = first_space.binding().sessions().clone();
    assert!(sessions.claim(claimed_session("delayed-id", "delayed", &cwd)));
    repository
        .commit_binding_state(first_scope, &sessions)
        .expect("persist delayed session");
    // The session is already running and already carries its Space's tag, which is what the
    // workspace reads membership from.
    first_sessions.lock().expect("seed the delayed session tag")[0].tag = MuxSessionTag {
        identity: Some("delayed-id".to_owned()),
        space: Some(first_space.remote_id().to_owned()),
    };
    let second_space = repository
        .create_space(
            "Second",
            "2",
            [0x22, 0x44, 0x66],
            false,
            SpaceMuxOverride {
                backend: Some(MultiplexerBackendConfig::Rmux),
                remote: SpaceRemoteOverride::Local,
            },
            config.multiplexer.hide_tmux_status,
        )
        .expect("create second Space")
        .expect("valid second Space");
    let second_id = second_space.id();
    let second_scope = second_space.binding().mux_scope();
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
    assert!(matches!(
        state.modal_dialog(),
        Some(ModalDialog::DitchSession(_))
    ));
    state.clear_last_error();
    state.apply_ditch_session_event(DitchSessionEvent::Ditch {
        session_id: "delayed".to_owned(),
        cwd: None,
        action: DitchAction::KillOnly,
    });
    assert!(state.activate_space_from_ui(second_id));
    assert!(state.binding_session_groups()[0].sessions.is_empty());
    release_tx.send(()).expect("release delayed ditch");

    for tick in 0..250 {
        state.update_frame(frame(Instant::now() + Duration::from_millis(tick)));
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(state.last_error().is_none(), "{:#?}", state.last_error());
    assert!(state.activate_space_from_ui(first_scope));
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
        .find(|space| space.id() == first_scope)
        .expect("first Space");
    let second = reopened
        .spaces()
        .iter()
        .find(|space| space.id() == second_scope)
        .expect("second Space");
    assert!(first.binding().sessions().is_empty());
    assert!(second.binding().sessions().is_empty());
}

#[test]
fn a_failed_placement_commit_preserves_the_live_and_durable_binding() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let config = BoottyConfig {
        config_path: config_path.clone(),
        multiplexer: bootty_config::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Native,
            ..bootty_config::config::MultiplexerConfig::default()
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
    let binding = &reopened.spaces()[0].binding();
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
        reopened.spaces()[0]
            .binding()
            .sessions()
            .backend_names()
            .contains(&original_name)
    );
}

#[test]
fn an_inactive_placement_update_rebuilds_before_activation() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let config = BoottyConfig {
        config_path: config_path.clone(),
        multiplexer: bootty_config::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Native,
            ..bootty_config::config::MultiplexerConfig::default()
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
        multiplexer: bootty_config::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Tmux,
            ..bootty_config::config::MultiplexerConfig::default()
        },
        ..BoottyConfig::default()
    };
    let (mut repository, snapshot) =
        WorkspaceRepository::open(&config_path).expect("workspace repository");
    let first_scope = snapshot.spaces()[0].binding().mux_scope();
    let first_binding = snapshot.spaces()[0].binding().clone();
    let mut sessions = first_binding.sessions().clone();
    assert!(sessions.claim(claimed_session(
        "persisted-first-id",
        "persisted-first",
        directory.path().to_str().expect("workspace path"),
    )));
    repository
        .commit_binding_state(first_scope, &sessions)
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
    let second_scope = second_space.binding().mux_scope();
    for (scope, name) in [
        (first_scope, "interrupted-first"),
        (second_scope, "interrupted-second"),
    ] {
        repository
            .begin_binding_membership_mutation(
                scope,
                &BindingMembershipMutation::Create {
                    identity: format!("{name}-id"),
                    session_name: name.to_owned(),
                    display_name: name.to_owned(),
                    explicit: true,
                    cwd: String::new(),
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
        .find(|space| space.id == first_scope)
        .expect("first Space")
        .clone();
    let second_space = spaces
        .iter()
        .find(|space| space.id == second_scope)
        .expect("second Space")
        .clone();
    let local_override = SpaceMuxOverride {
        backend: None,
        remote: SpaceRemoteOverride::Local,
    };
    state.update_frame(frame(Instant::now()));
    assert!(
        repository
            .pending_binding_membership_mutations(first_scope)
            .expect("read first pending operation")
            .is_empty(),
        "one active frame must resolve the first journal"
    );
    assert!(
        repository
            .pending_binding_membership_mutations(second_scope)
            .expect("read second pending operation")
            .is_empty(),
        "one frame must resolve the inactive binding journal"
    );
    let active_groups = state.binding_session_groups();
    assert_eq!(active_groups.len(), 1);
    assert_eq!(active_groups[0].scope, first_scope);
    assert!(active_groups[0].active);
    assert_eq!(create_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    // A backend change is never refused on account of bootty's own journal.
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
            .pending_binding_membership_mutations(second_scope)
            .expect("read second pending operation")
            .is_empty(),
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
            .all(|space| { space.binding().remote_override() == &SpaceRemoteOverride::Local })
    );
}

#[test]
fn a_deferred_profile_rebuild_preserves_the_intended_display_name() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let mut config = BoottyConfig {
        config_path: config_path.clone(),
        multiplexer: bootty_config::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Native,
            ..bootty_config::config::MultiplexerConfig::default()
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
    let scope = space.binding().mux_scope();
    repository
        .update_space(
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
    state.apply_picker_event(NewSessionPickerEvent::CreateSession {
        cwd: first_cwd.to_string_lossy().into_owned(),
    });
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

    state.apply_picker_event(NewSessionPickerEvent::CreateSession {
        cwd: second_cwd.to_string_lossy().into_owned(),
    });
    std::fs::write(
        &config_path,
        "[ssh-profiles.test]\nname = \"Changed\"\nhost = \"localhost\"\nprogram = \"ssh\"\n",
    )
    .expect("write changed profile");
    assert!(state.reload_config(&mut Vec::new()));
    assert!(
        !repository
            .pending_binding_membership_mutations(scope)
            .expect("read pending operation")
            .is_empty(),
        "profile reload must defer while the membership command is pending"
    );

    assert!((0..250).any(|tick| {
        state.update_frame(frame(started + Duration::from_millis(250 + tick)));
        std::thread::sleep(Duration::from_millis(1));
        repository
            .pending_binding_membership_mutations(scope)
            .expect("read pending operation")
            .is_empty()
    }));
    let (_, reopened) = WorkspaceRepository::open(&config_path).expect("reopen workspace");
    // The server needed a suffix to tell the two apart; bootty shows the name it asked for.
    let sessions = reopened.spaces()[0].binding().sessions();
    let claimed = sessions
        .sessions()
        .iter()
        .find(|session| session.backend_name == "project-2")
        .expect("the uniquified session");
    assert_eq!(claimed.label(), "project");
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
    let scope = space.binding().mux_scope();
    repository
        .update_space(
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
    let binding = space.binding().clone();
    let mut sessions = binding.sessions().clone();
    assert!(sessions.claim(claimed_session(
        "fallback-id",
        "persisted-local-fallback",
        directory.path().to_str().expect("workspace path"),
    )));
    repository
        .commit_binding_state(scope, &sessions)
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

/// A binding recorded unavailable when the app last closed must be able to come back. Marking it
/// with a *configured* error stopped it refreshing at all, so it could never succeed and never
/// clear the flag — and because reconciliation is what clears the membership journal, that also
/// left every later membership change failing on the journal's unique scope.
#[test]
fn a_binding_persisted_as_unavailable_recovers_on_a_successful_refresh() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let config = BoottyConfig {
        config_path: config_path.clone(),
        ..BoottyConfig::default()
    };
    let (mut repository, snapshot) =
        WorkspaceRepository::open(&config_path).expect("workspace repository");
    let scope = snapshot.spaces()[0].binding().mux_scope();
    repository
        .set_binding_restore_state(scope, true, None, None)
        .expect("persist the binding as unavailable");

    let repaint = Arc::new(|| {});
    let mut state =
        AppState::new(config, support::backends(), repaint, None, None).expect("app state");
    assert_eq!(
        state.space_summaries()[0].error.as_deref(),
        Some("binding unavailable; reconnect to restore it"),
        "the last session's failure is still reported"
    );

    let started = Instant::now();
    assert!(
        (0..250).any(|tick| {
            state.update_frame(frame(started + Duration::from_millis(250 + tick)));
            std::thread::sleep(Duration::from_millis(1));
            state.space_summaries()[0].error.is_none()
        }),
        "a refresh that works clears the flag: {:?}",
        state.space_summaries()[0].error
    );
}

/// A steady-state frame forks nothing.
///
/// Resolving a session's directory means asking `git` for its worktree root, which forks and blocks
/// the frame thread for as long as the child takes. The reconciler asks for every session's
/// directory on every frame, so without a memo that is one fork per session per frame and typing
/// visibly lags. `guard_frame_path` panics naming whatever spawns, so this fails at the offender
/// rather than as a slow frame nobody measures.
#[test]
fn steady_state_frames_do_not_fork_a_subprocess() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let config = BoottyConfig {
        config_path: config_path.clone(),
        multiplexer: bootty_config::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Tmux,
            ..bootty_config::config::MultiplexerConfig::default()
        },
        ..BoottyConfig::default()
    };
    let cwd = directory.path().to_string_lossy().into_owned();
    let (_, snapshot) = WorkspaceRepository::open(&config_path).expect("workspace repository");
    let space_tag = snapshot.spaces()[0].remote_id().to_owned();
    // Two sessions in one directory and one in another: the memo has to answer for a directory it
    // has already resolved, whoever asks for it.
    let sessions = Arc::new(Mutex::new(
        [
            ("work", cwd.clone()),
            ("review", cwd.clone()),
            ("docs", cwd),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (name, cwd))| MuxSession {
            anchor: MuxPaneAnchor {
                session_id: name.to_owned(),
                cwd: Some(cwd),
                ..MuxPaneAnchor::default()
            },
            id: name.to_owned(),
            name: name.to_owned(),
            active: index == 0,
            active_window_id: None,
            tag: MuxSessionTag {
                identity: Some(format!("{name}-id")),
                space: Some(space_tag.clone()),
            },
            windows: Vec::new(),
        })
        .collect::<Vec<_>>(),
    ));
    let backends = Arc::new(
        MuxBackendRegistry::from_app_providers(
            [Arc::new(RestoreProvider {
                kind: MuxBackendKind::Tmux,
                dispatch: MuxCommandDispatch::CallerThread,
                sessions: Arc::clone(&sessions),
                create_calls: Arc::new(AtomicUsize::new(0)),
                release: None,
                native_panes: false,
                selection_publication: SelectionPublicationPolicy::Direct,
            })],
            [MuxBackendKind::Tmux],
        )
        .expect("test backend registry"),
    );

    let mut state =
        AppState::new(config, backends, Arc::new(|| {}), None, None).expect("app state");
    // Settle first: claiming these sessions resolves each directory once, which is allowed to fork.
    for tick in 0..40 {
        state.update_frame(frame(Instant::now() + Duration::from_millis(tick)));
    }
    assert_eq!(
        state.binding_session_groups()[0].sessions.len(),
        3,
        "the Space claims the sessions its tag names"
    );

    let _guard = bootty_runtime::perf::guard_frame_path();
    for tick in 40..80 {
        state.update_frame(frame(Instant::now() + Duration::from_millis(tick)));
    }
}
