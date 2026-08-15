use std::{
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use bootty_app::commands::{
    AppCommandRequest, Caller, CommandCancellation, CommandCatalog, CommandDescriptor,
    CommandExecutor, CommandInvocation, CommandOutcome, CompactSchema, MutationClass, ResourceKind,
    app_command_channel,
};
use bootty_app::{
    app::{AppState, FrameInputs, ViewportSnapshot},
    command_extensions::CommandExtensionHost,
    config::BoottyConfig,
    control::ControlPlane,
    geometry::ViewTransform,
    renderer::RendererMetrics,
};

fn frame(now: Instant) -> FrameInputs {
    FrameInputs {
        now,
        stable_dt_ms: 1.0,
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
fn extension_commands_are_namespaced_and_cleared_together() {
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
            .register_extension("other", descriptor.clone(), handler.clone())
            .is_err()
    );
    catalog
        .register_extension("agent", descriptor, handler)
        .expect("register namespaced extension command");
    assert!(catalog.describe("agent.inspect").is_some());

    catalog.clear_extensions();
    assert!(catalog.describe("agent.inspect").is_none());
}

#[test]
fn destructive_policy_is_identical_for_core_and_extension_commands() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        ..BoottyConfig::default()
    };
    let mut state = AppState::new(config, Arc::new(|| {}), None, None).expect("app state");
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
        .register_extension(
            "test",
            descriptor,
            Arc::new(|_, _, _| {
                let (sender, receiver) = mpsc::channel();
                sender
                    .send(CommandOutcome::success())
                    .expect("send extension outcome");
                receiver
            }),
        )
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
    let package = directory.path().join("probe");
    std::fs::create_dir_all(&package).expect("create extension package");
    std::fs::write(
        package.join("extension.json"),
        r#"{"id":"probe","entrypoint":"main.luau"}"#,
    )
    .expect("write extension manifest");
    std::fs::write(
        package.join("main.luau"),
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
    let _host = CommandExtensionHost::load(
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
        CommandOutcome::Success { value, .. }
            if value["status"] == "failed" && value["code"] == "cancelled"
    ));
}
