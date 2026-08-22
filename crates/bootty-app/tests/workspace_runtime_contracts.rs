use std::{
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use bootty_app::{
    app::{AppState, FrameInputs, ViewportSnapshot},
    commands::{AppCommandRequest, Caller, CommandCancellation, CommandInvocation, CommandOutcome},
    config::{BoottyConfig, MultiplexerBackendConfig, SshProfileConfig},
    geometry::ViewTransform,
    renderer::RendererMetrics,
    ui::new_session_picker::{NewMuxSessionDialog, NewSessionPickerEvent},
    workspace::{
        BindingMembershipMutation, RemoteSpaceRef, SpaceMuxOverride, SpaceRemoteOverride,
        WorkspaceRepository,
    },
};
use rusqlite::Connection;

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
    let mut state = AppState::new(config, repaint, None, None).expect("app state");
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
    let mut state = AppState::new(config, repaint, None, None).expect("app state");
    let commands = state.app_command_sender(Caller::Socket);
    let (response, outcomes) = mpsc::channel();
    commands
        .try_send(AppCommandRequest {
            invocation: CommandInvocation::from_action("new_tab", Caller::Socket),
            deadline: Instant::now() + Duration::from_secs(2),
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
fn rebuilding_one_binding_preserves_another_bindings_pending_recovery() {
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
    let (mut repository, snapshot) =
        WorkspaceRepository::open(&config_path).expect("workspace repository");
    let first_scope = snapshot.spaces()[0].bindings()[0].mux_scope();
    let second_space = repository
        .create_space(
            "Second",
            "2",
            [0x22, 0x44, 0x66],
            false,
            SpaceMuxOverride::default(),
            &config.multiplexer,
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
    let mut state = AppState::new(config, repaint, None, None).expect("app state");
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

    let started = Instant::now();
    let first_recovered = (0..250).any(|tick| {
        state.update_frame(frame(started + Duration::from_millis(tick)));
        std::thread::sleep(Duration::from_millis(1));
        repository
            .pending_binding_membership_mutation(first_scope)
            .expect("read first pending operation")
            .is_none()
    });
    assert!(
        first_recovered,
        "startup refresh must resolve the first journal"
    );
    assert!(
        repository
            .pending_binding_membership_mutation(second_scope)
            .expect("read second pending operation")
            .is_some(),
        "the inactive binding must keep its own recovery"
    );
    assert!(state.update_space_from_ui(
        first_space.id,
        &first_space.name,
        &first_space.icon,
        first_space.color,
        first_space.tint_sidebar,
        local_override.clone(),
    ));
    assert!(!state.update_space_from_ui(
        second_space.id,
        &second_space.name,
        &second_space.icon,
        second_space.color,
        second_space.tint_sidebar,
        local_override.clone(),
    ));

    assert!(state.activate_space_from_ui(second_space.id));
    let second_recovered = (0..250).any(|tick| {
        state.update_frame(frame(started + Duration::from_millis(250 + tick)));
        std::thread::sleep(Duration::from_millis(1));
        repository
            .pending_binding_membership_mutation(second_scope)
            .expect("read second pending operation")
            .is_none()
    });
    assert!(
        second_recovered,
        "activation must resolve the second journal"
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
    let mut state = AppState::new(config, repaint, None, None).expect("app state");
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

    let repaint = Arc::new(|| {});
    let mut state = AppState::new(config, repaint, None, None).expect("app state");
    assert_eq!(
        state.space_summaries()[0].error.as_deref(),
        Some("SSH profile 'development' is unavailable")
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
