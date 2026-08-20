use std::{
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use bootty_app::commands::{
    AppCommandRequest, Caller, CommandCancellation, CommandCatalog, CommandDescriptor,
    CommandExecutor, CommandInvocation, CommandOutcome, CommandTarget, CompactSchema,
    ExtensionGenerationCandidate, ExtensionGenerationToken, MutationClass, ResourceKind,
    app_command_channel_with_repaint,
};
use bootty_app::{
    app::{AppState, FrameInputs, ModalDialog, ViewportSnapshot},
    command_extensions::{ExtensionHost, ModuleIdentity},
    config::{BoottyConfig, MultiplexerBackendConfig},
    control::ControlPlane,
    geometry::ViewTransform,
    renderer::RendererMetrics,
    ui::new_session_picker::NewSessionPickerEvent,
};

mod support;

fn app_command_channel(
    capacity: usize,
) -> (
    bootty_app::commands::AppCommandSender,
    bootty_app::commands::AppCommandReceiver,
) {
    app_command_channel_with_repaint(capacity, Arc::new(|| {}))
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

#[test]
fn core_commands_share_one_typed_catalog_contract() {
    let catalog = CommandCatalog::default();
    let read = catalog
        .describe("terminal.read")
        .expect("terminal read command");
    let write = catalog
        .describe("terminal.write")
        .expect("terminal write command");

    assert_eq!(read.mutation, MutationClass::Read);
    assert_eq!(read.target, Some(ResourceKind::Terminal));
    assert_eq!(write.mutation, MutationClass::Write);
    assert_eq!(write.target, Some(ResourceKind::Terminal));
    let resource = catalog
        .describe("resource.current")
        .expect("current resource command");
    assert_eq!(resource.mutation, MutationClass::Read);
    assert_eq!(resource.target, None);
    assert!(matches!(
        catalog.resolve(CommandInvocation::from_action(
            "terminal.write",
            Caller::Socket,
        )),
        Err(CommandOutcome::Failed { code, .. }) if code == "invalid_arguments"
    ));
    assert!(matches!(
        catalog.resolve(CommandInvocation::from_action(
            "missing.command",
            Caller::Socket,
        )),
        Err(CommandOutcome::Failed { code, .. }) if code == "unknown_command"
    ));
}

#[test]
fn discovered_resource_target_cannot_retarget_a_replacement_binding() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        multiplexer: bootty_app::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Native,
            ..bootty_app::config::MultiplexerConfig::default()
        },
        ..BoottyConfig::default()
    };
    let started = Instant::now();
    let mut first = AppState::new(
        config.clone(),
        support::backends(),
        Arc::new(|| {}),
        None,
        None,
    )
    .expect("first app state");
    let current = submit_command(
        &mut first,
        CommandInvocation::new(
            "resource.current",
            vec!["binding".to_owned()],
            Caller::Socket,
        ),
        started,
    );
    let CommandOutcome::Success { value, .. } = current else {
        panic!("current binding outcome: {current:?}");
    };
    let target: CommandTarget =
        serde_json::from_value(value["target"].clone()).expect("current binding target");
    drop(first);

    let mut replacement = AppState::new(config, support::backends(), Arc::new(|| {}), None, None)
        .expect("replacement app state");
    let mut invocation = CommandInvocation::new("edit_space", Vec::new(), Caller::Socket);
    invocation.target = Some(target);
    let outcome = submit_command(
        &mut replacement,
        invocation,
        started + Duration::from_millis(20),
    );
    assert!(matches!(outcome, CommandOutcome::StaleTarget { .. }));
}

#[test]
fn native_split_command_publishes_the_binding_owned_layout() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        multiplexer: bootty_app::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Native,
            ..bootty_app::config::MultiplexerConfig::default()
        },
        ..BoottyConfig::default()
    };
    let started = Instant::now();
    let mut state =
        AppState::new(config, support::backends(), Arc::new(|| {}), None, None).expect("app state");
    let open = submit_command(
        &mut state,
        CommandInvocation::from_action("new_mux_session", Caller::Socket),
        started,
    );
    assert!(matches!(open, CommandOutcome::Success { .. }), "{open:?}");
    let ModalDialog::NewSession(dialog) = state.take_modal_dialog().expect("new session dialog")
    else {
        panic!("expected new session dialog");
    };
    state.apply_picker_event(
        dialog,
        NewSessionPickerEvent::CreateSession {
            cwd: directory.path().to_string_lossy().into_owned(),
        },
    );
    for tick in 1..5 {
        state.update_frame(frame(started + Duration::from_millis(tick)));
    }

    let outcome = submit_command(
        &mut state,
        CommandInvocation::from_action("split_right", Caller::Socket),
        started + Duration::from_millis(10),
    );

    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert!(state.native_multi_pane());
    assert_eq!(
        state
            .pane_rects(
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(200.0, 100.0),),
                4.0
            )
            .len(),
        2
    );
    assert!(state.focused_pane().is_some());
}

#[test]
fn native_window_actions_use_the_binding_owned_plan() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        multiplexer: bootty_app::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Native,
            ..bootty_app::config::MultiplexerConfig::default()
        },
        ..BoottyConfig::default()
    };
    let started = Instant::now();
    let mut state =
        AppState::new(config, support::backends(), Arc::new(|| {}), None, None).expect("app state");
    let open = submit_command(
        &mut state,
        CommandInvocation::from_action("new_mux_session", Caller::Socket),
        started,
    );
    assert!(matches!(open, CommandOutcome::Success { .. }), "{open:?}");
    let ModalDialog::NewSession(dialog) = state.take_modal_dialog().expect("new session dialog")
    else {
        panic!("expected new session dialog");
    };
    state.apply_picker_event(
        dialog,
        NewSessionPickerEvent::CreateSession {
            cwd: directory.path().to_string_lossy().into_owned(),
        },
    );
    for tick in 1..5 {
        state.update_frame(frame(started + Duration::from_millis(tick)));
    }

    let session = state.mux().sessions()[0].clone();
    let first_window = session.windows[0].id.clone();
    assert!(state.new_tab_for_window_from_ui(&session.id, &first_window));
    for tick in 5..10 {
        state.update_frame(frame(started + Duration::from_millis(tick)));
    }

    let session = state
        .mux()
        .sessions()
        .iter()
        .find(|candidate| candidate.id == session.id)
        .expect("created session");
    assert_eq!(session.windows.len(), 2);
    let second_window = session
        .windows
        .iter()
        .find(|window| window.id != first_window)
        .expect("new window")
        .id
        .clone();
    let session_id = session.id.clone();

    assert!(state.activate_relative_window_from_ui(&session_id, &first_window, 1));
    assert_eq!(state.mux().selected_window(), Some(second_window.as_str()));
    assert!(state.activate_last_window_from_ui(&session_id));
    assert_eq!(state.mux().selected_window(), Some(first_window.as_str()));
    assert!(state.move_window_from_ui(&session_id, &second_window, -1));
    assert!(state.close_pane_for_window_from_ui(&session_id, &second_window));
}

#[test]
fn extension_commands_are_namespaced_and_removed_by_generation() {
    let catalog = CommandCatalog::default();
    let descriptor = CommandDescriptor {
        id: "agent.inspect".to_owned(),
        title: "Inspect Agent".to_owned(),
        description: "Inspect one agent session.".to_owned(),
        mutation: MutationClass::Read,
        arguments: CompactSchema::default(),
        target: Some(ResourceKind::Session),
        palette: false,
    };
    let handler = Arc::new(|_, _, _| {
        let (sender, receiver) = mpsc::channel();
        sender
            .send(CommandOutcome::success())
            .expect("send extension outcome");
        receiver
    });

    assert!(
        catalog
            .publish_extension_generation(ExtensionGenerationCandidate {
                identity: ModuleIdentity::parse("other/agent.luau").expect("module identity"),
                generation: 7,
                token: ExtensionGenerationToken::new(),
                commands: vec![(descriptor.clone(), handler.clone())],
                topics: Vec::new(),
                surfaces: Vec::new(),
            })
            .is_err()
    );
    catalog
        .publish_extension_generation(ExtensionGenerationCandidate {
            identity: ModuleIdentity::parse("agent.luau").expect("module identity"),
            generation: 7,
            token: ExtensionGenerationToken::new(),
            commands: vec![(descriptor, handler)],
            topics: Vec::new(),
            surfaces: Vec::new(),
        })
        .expect("register namespaced extension command");
    assert!(catalog.describe("agent.inspect").is_some());

    catalog.remove_extension_generation("agent.luau", 6);
    assert!(catalog.describe("agent.inspect").is_some());
    catalog.remove_extension_generation("agent.luau", 7);
    assert!(catalog.describe("agent.inspect").is_none());
}

#[test]
fn destructive_policy_is_identical_for_core_and_extension_commands() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        ..BoottyConfig::default()
    };
    let mut state =
        AppState::new(config, support::backends(), Arc::new(|| {}), None, None).expect("app state");
    let descriptor = CommandDescriptor {
        id: "test.destroy".to_owned(),
        title: "Destroy Test".to_owned(),
        description: String::new(),
        mutation: MutationClass::Destructive,
        arguments: CompactSchema::default(),
        target: None,
        palette: false,
    };
    state
        .command_catalog()
        .publish_extension_generation(ExtensionGenerationCandidate {
            identity: ModuleIdentity::parse("test.luau").expect("module identity"),
            generation: 1,
            token: ExtensionGenerationToken::new(),
            commands: vec![(
                descriptor,
                Arc::new(|_, _, _| {
                    let (sender, receiver) = mpsc::channel();
                    sender
                        .send(CommandOutcome::success())
                        .expect("send extension outcome");
                    receiver
                }),
            )],
            topics: Vec::new(),
            surfaces: Vec::new(),
        })
        .expect("register extension command");

    let started = Instant::now();
    for (caller, expected_confirmation) in [
        (Caller::CommandPalette, false),
        (Caller::Keybinding, false),
        (Caller::Cli, true),
        (Caller::Socket, true),
        (Caller::Luau, true),
    ] {
        let commands = state.app_command_sender(caller);
        let (response, outcomes) = mpsc::channel();
        commands
            .try_send(AppCommandRequest {
                invocation: CommandInvocation::from_action("test.destroy", caller),
                deadline: Instant::now() + Duration::from_secs(1),
                cancellation: CommandCancellation::new(),
                response,
            })
            .expect("submit extension command");
        let outcome = (0..10)
            .find_map(|tick| {
                state.update_frame(frame(started + Duration::from_millis(tick)));
                outcomes.try_recv().ok()
            })
            .expect("extension command outcome");
        assert_eq!(
            matches!(outcome, CommandOutcome::ConfirmationRequired { .. }),
            expected_confirmation,
            "caller {caller:?}"
        );
    }
}

#[test]
fn luau_nested_commands_keep_typed_fields_deadline_and_cancellation() {
    let directory = tempfile::tempdir().expect("temporary extensions");
    std::fs::write(
        directory.path().join("probe.luau"),
        r#"
bootty.commands.register({
    id = "probe.forward",
    title = "Forward",
    arguments = {{ name = "command", type = "string", required = true }},
}, function(context)
    local target = { kind = "terminal", handle = "opaque", generation = "7" }
    return bootty.commands.invoke({
        command = context.arguments[1],
        arguments = { "payload" },
        target = target,
        confirmation = {
            command = context.arguments[1],
            arguments = { "payload" },
            target = target,
        },
    })
end)
"#,
    )
    .expect("write extension source");

    let catalog = Arc::new(CommandCatalog::default());
    let (sender, receiver) = app_command_channel(4);
    let _host = ExtensionHost::load(
        directory.path(),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        ControlPlane::default(),
    );
    let resolved = catalog
        .resolve(CommandInvocation::from_action(
            "probe.forward:ignore",
            Caller::Socket,
        ))
        .expect("resolve extension command");
    let CommandExecutor::Extension(handler) = resolved.executor else {
        panic!("extension executor");
    };
    let deadline = Instant::now() + Duration::from_secs(1);
    let cancellation = CommandCancellation::new();
    let outcome = handler(resolved.invocation, deadline, cancellation.clone());
    let nested = (0..100)
        .find_map(|_| {
            let request = receiver.try_recv().ok();
            if request.is_none() {
                std::thread::sleep(Duration::from_millis(1));
            }
            request
        })
        .expect("nested app command");

    assert_eq!(nested.invocation.command, "ignore");
    assert_eq!(nested.invocation.arguments, ["payload"]);
    assert_eq!(nested.invocation.caller, Caller::Luau);
    assert_eq!(nested.deadline, deadline);
    assert_eq!(
        nested.invocation.target.as_ref().map(|target| (
            target.kind,
            target.handle.as_str(),
            target.generation,
        )),
        Some((ResourceKind::Terminal, "opaque", 7))
    );
    assert_eq!(
        nested.invocation.confirmation,
        Some(nested.invocation.confirmation())
    );
    nested
        .response
        .send(CommandOutcome::Success {
            value: serde_json::json!({"source": "app"}),
            warnings: Vec::new(),
        })
        .expect("complete nested command");
    let outer = outcome
        .recv_timeout(Duration::from_secs(1))
        .expect("extension outcome");
    assert!(matches!(
        outer,
        CommandOutcome::Success { value, .. }
            if value["status"] == "success" && value["value"]["source"] == "app"
    ));

    let resolved = catalog
        .resolve(CommandInvocation::from_action(
            "probe.forward:ignore",
            Caller::Socket,
        ))
        .expect("resolve extension command again");
    let CommandExecutor::Extension(handler) = resolved.executor else {
        panic!("extension executor");
    };
    let cancellation = CommandCancellation::new();
    let outcome = handler(
        resolved.invocation,
        Instant::now() + Duration::from_secs(1),
        cancellation.clone(),
    );
    let nested = (0..100)
        .find_map(|_| {
            let request = receiver.try_recv().ok();
            if request.is_none() {
                std::thread::sleep(Duration::from_millis(1));
            }
            request
        })
        .expect("nested cancellable command");
    cancellation.cancel();
    assert!(nested.cancellation.is_cancelled());
    let outer = outcome
        .recv_timeout(Duration::from_secs(1))
        .expect("cancelled extension outcome");
    assert!(matches!(
        outer,
        CommandOutcome::Failed { code, .. } if code == "cancelled"
    ));
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
    (0..100)
        .find_map(|tick| {
            state.update_frame(frame(started + Duration::from_millis(tick)));
            outcomes.try_recv().ok()
        })
        .expect("command outcome")
}
