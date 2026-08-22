#![cfg(unix)]

use std::{
    fs,
    process::{Command, Output, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use bootty_app::{
    app::{AppState, FrameInputs, ViewportSnapshot},
    command_extensions::{ExtensionHost, ModuleIdentity},
    commands::{
        AppCommandReceiver, Caller, CommandCatalog, CommandOutcome, CommandTarget, Confirmation,
        ExtensionGenerationCandidate, ExtensionGenerationToken, ResourceKind,
        app_command_channel_with_repaint,
    },
    config::{BoottyConfig, MultiplexerBackendConfig},
    control::{ControlPlane, ControlServer},
    geometry::ViewTransform,
    renderer::RendererMetrics,
};
use serde_json::{Value, json};

const HELPER_ENV: &str = "BOOTTY_CLI_CONTROL_TEST_HELPER";

fn app_command_channel(
    capacity: usize,
) -> (bootty_app::commands::AppCommandSender, AppCommandReceiver) {
    app_command_channel_with_repaint(capacity, Arc::new(|| {}))
}

#[test]
fn one_executable_uses_the_live_owner_without_a_second_gui() {
    let runtime = tempfile::tempdir().expect("temporary runtime directory");
    let status = Command::new(std::env::current_exe().expect("current test executable"))
        .args(["--exact", "cli_control_helper"])
        .env(HELPER_ENV, "1")
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("RMUX_TMPDIR", runtime.path())
        .status()
        .expect("run isolated CLI control check");

    assert!(status.success());
}

#[test]
fn cli_control_helper() {
    if std::env::var_os(HELPER_ENV).is_none() {
        return;
    }

    let (sender, receiver) = app_command_channel(4);
    let control_plane = ControlPlane::default();
    let catalog = Arc::new(CommandCatalog::default());
    let server = ControlServer::spawn(
        "main".to_owned(),
        sender.for_caller(Caller::Socket),
        Arc::clone(&catalog),
        control_plane.clone(),
    )
    .expect("start control owner");

    let mut child = spawn_bootty(&["command", "ignore"]);
    let request = receive_request(&receiver);
    assert_eq!(request.invocation.command, "ignore");
    assert!(request.invocation.arguments.is_empty());
    assert_eq!(request.invocation.caller, Caller::Socket);
    request
        .response
        .send(CommandOutcome::success())
        .expect("complete CLI command");
    assert!(child.wait().expect("wait for CLI command").success());

    let commands = run_bootty(&["--json", "commands"]);
    assert!(commands.status.success());
    let commands = rpc_result(&commands);
    assert!(commands.as_array().is_some_and(|commands| {
        commands
            .iter()
            .any(|command| command.get("id") == Some(&json!("terminal.read")))
            && commands
                .iter()
                .any(|command| command.get("id") == Some(&json!("terminal.write")))
    }));

    let described = run_bootty(&["--json", "describe", "terminal.write"]);
    assert!(described.status.success());
    assert_eq!(rpc_result(&described)["target"], json!("terminal"));

    let target = CommandTarget {
        kind: ResourceKind::Terminal,
        handle: "terminal-owner".to_owned(),
        generation: 17,
    };
    let current = spawn_bootty(&["--json", "resource.current", "--kind", "terminal"]);
    let request = receive_request(&receiver);
    assert_eq!(request.invocation.command, "resource.current");
    assert_eq!(request.invocation.arguments, ["terminal"]);
    request
        .response
        .send(CommandOutcome::Success {
            value: json!({"target": target}),
            warnings: Vec::new(),
        })
        .expect("complete resource discovery");
    let current = current
        .wait_with_output()
        .expect("wait for resource discovery");
    assert!(current.status.success());
    assert_eq!(rpc_result(&current)["value"]["target"], json!(target));

    let target_argument = format!("{}@{}", target.handle, target.generation);
    let read = spawn_bootty(&["--json", "terminal.read", "--target", &target_argument]);
    let request = receive_request(&receiver);
    assert_eq!(request.invocation.command, "terminal.read");
    assert_eq!(request.invocation.target.as_ref(), Some(&target));
    assert_eq!(request.invocation.caller, Caller::Socket);
    request
        .response
        .send(CommandOutcome::Success {
            value: json!({"cols": 4, "rows": 1, "text": "read"}),
            warnings: Vec::new(),
        })
        .expect("complete terminal read");
    let read = read.wait_with_output().expect("wait for terminal read");
    assert!(read.status.success());
    assert_eq!(rpc_result(&read)["value"]["text"], json!("read"));

    let write = spawn_bootty(&[
        "--json",
        "terminal.write",
        "--text",
        "hello",
        "--target",
        &target_argument,
    ]);
    let request = receive_request(&receiver);
    assert_eq!(request.invocation.command, "terminal.write");
    assert_eq!(request.invocation.arguments, ["hello"]);
    assert_eq!(request.invocation.target.as_ref(), Some(&target));
    request
        .response
        .send(CommandOutcome::success())
        .expect("complete terminal write");
    assert!(
        write
            .wait_with_output()
            .expect("wait for terminal write")
            .status
            .success()
    );

    let binding_target = CommandTarget {
        kind: ResourceKind::Binding,
        handle: "binding-owner".to_owned(),
        generation: 23,
    };
    let binding_argument = format!("{}@{}", binding_target.handle, binding_target.generation);
    let destructive = spawn_bootty(&[
        "--json",
        "close_space",
        "--target",
        &binding_argument,
        "--yes",
    ]);
    let request = receive_request(&receiver);
    assert_eq!(request.invocation.command, "close_space");
    assert_eq!(request.invocation.target.as_ref(), Some(&binding_target));
    assert!(request.invocation.confirmation.is_none());
    let confirmation = Confirmation {
        command: "close_space".to_owned(),
        arguments: Vec::new(),
        target: Some(binding_target.clone()),
    };
    request
        .response
        .send(CommandOutcome::ConfirmationRequired {
            confirmation: Box::new(confirmation.clone()),
        })
        .expect("request destructive confirmation");
    let request = receive_request(&receiver);
    assert_eq!(request.invocation.confirmation, Some(confirmation));
    assert_eq!(request.invocation.target, Some(binding_target));
    request
        .response
        .send(CommandOutcome::success())
        .expect("complete confirmed command");
    assert!(
        destructive
            .wait_with_output()
            .expect("wait for destructive command")
            .status
            .success()
    );

    let subscribed = run_bootty(&["--json", "events", "subscribe", "command.completed"]);
    assert!(subscribed.status.success());
    let subscribed = rpc_result(&subscribed);
    let subscription = subscribed["subscription"]
        .as_str()
        .expect("subscription identifier")
        .to_owned();

    let started = run_bootty(&["--json", "command", "--detach", "terminal.read"]);
    assert!(
        started.status.success(),
        "detached command failed: stdout={}; stderr={}",
        String::from_utf8_lossy(&started.stdout),
        String::from_utf8_lossy(&started.stderr)
    );
    let started = rpc_result(&started);
    let task = started["task"]["id"]
        .as_str()
        .expect("task identifier")
        .to_owned();
    let request = receive_request(&receiver);
    let cancellation = request.cancellation.clone();
    assert!(!cancellation.is_cancelled());
    let status = run_bootty(&["--json", "task", "status", &task]);
    assert!(status.status.success());
    assert_eq!(
        rpc_result(&status)["task"]["state"]["status"],
        json!("running")
    );
    let cancelling = run_bootty(&["--json", "task", "cancel", &task]);
    assert!(cancelling.status.success());
    let cancelling = rpc_result(&cancelling);
    assert_eq!(cancelling["task"]["state"]["status"], json!("cancelling"));
    assert!(cancellation.is_cancelled());
    drop(request.response);

    let deadline = Instant::now() + CLI_BUDGET;
    loop {
        let status = run_bootty(&["--json", "task", "status", &task]);
        assert!(status.status.success());
        let status = rpc_result(&status);
        if status["task"]["state"]["status"] == json!("completed") {
            assert_eq!(status["task"]["state"]["outcome"]["code"], json!("-32003"));
            break;
        }
        assert!(Instant::now() < deadline, "cancelled task did not complete");
        thread::sleep(Duration::from_millis(5));
    }

    let events = run_bootty(&["--json", "events", "poll", &subscription, "--cursor", "0"]);
    assert!(events.status.success());
    let events = rpc_result(&events);
    assert_eq!(events["events"][0]["topic"], json!("command.completed"));
    assert_eq!(
        events["events"][0]["payload"]["command"],
        json!("terminal.read")
    );

    let first_topic_generation = ExtensionGenerationToken::new();
    catalog
        .publish_extension_generation(ExtensionGenerationCandidate {
            identity: ModuleIdentity::parse("test.luau").expect("module identity"),
            generation: 1,
            token: first_topic_generation.clone(),
            commands: Vec::new(),
            topics: vec!["test.changed".to_owned()],
            surfaces: Vec::new(),
        })
        .expect("publish test topic");
    let subscribed = run_bootty(&["--json", "events", "subscribe", "test.changed"]);
    assert!(subscribed.status.success());
    let subscribed = rpc_result(&subscribed);
    let overflowed = subscribed["subscription"]
        .as_str()
        .expect("overflow subscription")
        .to_owned();
    for sequence in 0..65 {
        control_plane
            .publish_extension_event(
                &catalog,
                "test.luau",
                1,
                "test.changed",
                json!({"sequence": sequence}),
            )
            .expect("publish bounded event");
    }
    catalog
        .publish_extension_generation(ExtensionGenerationCandidate {
            identity: ModuleIdentity::parse("test.luau").expect("module identity"),
            generation: 2,
            token: ExtensionGenerationToken::new(),
            commands: Vec::new(),
            topics: vec!["test.changed".to_owned()],
            surfaces: Vec::new(),
        })
        .expect("replace test topic generation");
    assert!(!first_topic_generation.is_active());
    assert_eq!(
        control_plane.publish_extension_event(
            &catalog,
            "test.luau",
            1,
            "test.changed",
            json!({"sequence": "stale"}),
        ),
        Err("extension event topic is not active".to_owned())
    );
    let response = run_bootty(&["--json", "events", "poll", &overflowed, "--cursor", "0"]);
    assert!(!response.status.success());
    let error = rpc_error(&response);
    assert_eq!(error["code"], json!(-32005));
    assert_eq!(error["message"], json!("event rebase required"));
    assert_eq!(error["data"]["rebase"], json!("snapshot"));
    let unsubscribed = run_bootty(&["--json", "events", "unsubscribe", &overflowed]);
    assert!(unsubscribed.status.success());
    assert_eq!(rpc_result(&unsubscribed)["unsubscribed"], json!(overflowed));

    let mut bare = Command::new(env!("CARGO_BIN_EXE_bootty"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start bare Bootty invocation");
    let deadline = Instant::now() + CLI_BUDGET;
    let status = loop {
        if let Some(status) = bare.try_wait().expect("poll bare invocation") {
            break status;
        }
        if Instant::now() >= deadline {
            bare.kill().expect("stop unexpected second GUI");
            panic!("bare Bootty tried to open a second GUI");
        }
        thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success());

    drop(server);

    let workspace = tempfile::tempdir().expect("temporary native workspace");
    let extension_root = workspace.path().join("extensions");
    fs::create_dir(&extension_root).expect("create native extension root");
    let extension_module = extension_root.join("probe.luau");
    fs::write(&extension_module, extension_source(1)).expect("write first extension generation");
    let config = BoottyConfig {
        config_path: workspace.path().join("config.toml"),
        multiplexer: bootty_app::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Native,
            ..bootty_app::config::MultiplexerConfig::default()
        },
        ..BoottyConfig::default()
    };
    let mut state = AppState::new(config, Arc::new(|| {}), None, None).expect("native app state");
    let native_control_plane = ControlPlane::default();
    let mut extension_host = ExtensionHost::load(
        &extension_root,
        state.command_catalog(),
        state.app_command_sender(Caller::Luau),
        native_control_plane.clone(),
    );
    let native_server = ControlServer::spawn(
        "main".to_owned(),
        state.app_command_sender(Caller::Socket),
        state.command_catalog(),
        native_control_plane,
    )
    .expect("start native control owner");
    let subscribed = run_bootty(&["--json", "events", "subscribe", "probe.changed"]);
    assert!(subscribed.status.success());
    let extension_subscription = rpc_result(&subscribed)["subscription"]
        .as_str()
        .expect("extension subscription identifier")
        .to_owned();
    let extension = run_bootty_with_state(&mut state, &["--json", "command", "probe.echo"]);
    assert!(extension.status.success());
    assert_eq!(rpc_result(&extension)["value"]["generation"], json!(1));
    let events = run_bootty(&[
        "--json",
        "events",
        "poll",
        &extension_subscription,
        "--cursor",
        "0",
    ]);
    assert!(events.status.success());
    assert_eq!(
        rpc_result(&events)["events"][0]["payload"],
        json!({"generation": 1})
    );

    let agent_subscription = run_bootty(&[
        "--json",
        "events",
        "subscribe",
        "agents.pi.event",
        "agents.codex.event",
    ]);
    assert!(agent_subscription.status.success());
    let agent_subscription = rpc_result(&agent_subscription)["subscription"]
        .as_str()
        .expect("agent event subscription")
        .to_owned();
    let pi_event = json!({
        "type": "tool_execution_start",
        "sessionId": "pi/native:17",
        "toolCallId": "tool/opaque",
        "toolName": "read"
    })
    .to_string();
    let pi = run_bootty_with_state(
        &mut state,
        &["--json", "command", "agents.pi.ingest", &pi_event],
    );
    assert!(pi.status.success());
    assert_eq!(
        rpc_result(&pi)["value"]["session_id"],
        json!("pi/native:17")
    );
    let codex_event = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "codex/native:23",
        "turn_id": "turn/opaque",
        "tool_name": "Bash",
        "transcript_path": "/not/read"
    })
    .to_string();
    let codex = run_bootty_with_state(
        &mut state,
        &["--json", "command", "agents.codex.ingest", &codex_event],
    );
    assert!(codex.status.success());
    assert_eq!(
        rpc_result(&codex)["value"]["thread_id"],
        json!("codex/native:23")
    );
    let agent_events = run_bootty(&[
        "--json",
        "events",
        "poll",
        &agent_subscription,
        "--cursor",
        "0",
    ]);
    assert!(agent_events.status.success());
    let agent_events = rpc_result(&agent_events)["events"]
        .as_array()
        .expect("agent events")
        .clone();
    assert!(agent_events.iter().any(|event| {
        event["topic"] == json!("agents.pi.event")
            && event["payload"]["payload"]["toolCallId"] == json!("tool/opaque")
    }));
    assert!(agent_events.iter().any(|event| {
        event["topic"] == json!("agents.codex.event")
            && event["payload"]["payload"]["turn_id"] == json!("turn/opaque")
    }));

    fs::write(&extension_module, extension_source(2)).expect("write second extension generation");
    extension_host.refresh(Instant::now() + Duration::from_secs(2));
    let extension = run_bootty_with_state(&mut state, &["--json", "command", "probe.echo"]);
    assert!(extension.status.success());
    assert_eq!(rpc_result(&extension)["value"]["generation"], json!(2));

    let native = run_bootty_with_state(&mut state, &["--json", "command", "new_tab"]);
    assert!(native.status.success());
    let outcome = rpc_result(&native);
    assert_eq!(outcome["status"], json!("success"));
    assert_eq!(outcome["value"]["created"]["kind"], json!("session"));
    assert!(
        outcome["value"]["created"]["handle"]
            .as_str()
            .is_some_and(|handle| !handle.is_empty())
    );
    assert!(
        state
            .binding_session_groups()
            .iter()
            .any(|group| !group.sessions.is_empty())
    );

    let current = run_bootty_with_state(
        &mut state,
        &["--json", "resource.current", "--kind", "terminal"],
    );
    assert!(current.status.success());
    let target = rpc_result(&current)["value"]["target"].clone();
    assert_eq!(target["kind"], json!("terminal"));
    let target_argument = format!(
        "{}@{}",
        target["handle"].as_str().expect("terminal target handle"),
        target["generation"]
            .as_str()
            .expect("terminal target generation")
    );

    let marker = "bootty-control-real-terminal-marker";
    let shell_input = format!("printf '{marker}\\n'\n");
    let written = run_bootty_with_state(
        &mut state,
        &[
            "--json",
            "terminal.write",
            "--text",
            &shell_input,
            "--target",
            &target_argument,
        ],
    );
    assert!(written.status.success());
    assert_eq!(rpc_result(&written)["status"], json!("success"));

    let deadline = Instant::now() + CLI_BUDGET;
    loop {
        let read = run_bootty_with_state(
            &mut state,
            &["--json", "terminal.read", "--target", &target_argument],
        );
        assert!(read.status.success());
        let text = rpc_result(&read)["value"]["text"]
            .as_str()
            .expect("terminal text")
            .to_owned();
        if text.contains(marker) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "real terminal did not publish the written marker"
        );
        thread::sleep(Duration::from_millis(10));
    }
    drop(native_server);
}

fn extension_source(generation: u64) -> String {
    format!(
        r#"
bootty.events.register("probe.changed")
bootty.commands.register({{ id = "probe.echo", title = "Echo" }}, function()
    bootty.events.publish("probe.changed", {{ generation = {generation} }})
    return {{ generation = {generation} }}
end)
"#
    )
}

fn spawn_bootty(arguments: &[&str]) -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_bootty"))
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start Bootty CLI")
}

fn run_bootty(arguments: &[&str]) -> Output {
    spawn_bootty(arguments)
        .wait_with_output()
        .expect("wait for Bootty CLI")
}

fn run_bootty_with_state(state: &mut AppState, arguments: &[&str]) -> Output {
    let mut child = spawn_bootty(arguments);
    let started = Instant::now();
    loop {
        state.update_frame(frame(Instant::now()));
        if child.try_wait().expect("poll Bootty CLI").is_some() {
            return child.wait_with_output().expect("collect Bootty CLI output");
        }
        assert!(
            started.elapsed() < CLI_BUDGET,
            "Bootty CLI command did not complete"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

/// Wall-clock budget for a spawned CLI to reach the app command channel.
///
/// A loaded parallel run can starve the child for seconds, so the budget only
/// has to be generous enough never to expire on a healthy run.
const CLI_BUDGET: Duration = Duration::from_secs(30);

fn receive_request(receiver: &AppCommandReceiver) -> bootty_app::commands::AppCommandRequest {
    let deadline = Instant::now() + CLI_BUDGET;
    loop {
        if let Ok(request) = receiver.try_recv() {
            return request;
        }
        assert!(
            Instant::now() < deadline,
            "CLI did not reach app command channel"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn rpc_result(output: &Output) -> Value {
    let response: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "decode CLI JSON: {error}; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(response["jsonrpc"], json!("2.0"));
    assert!(response.get("error").is_none());
    response["result"].clone()
}

fn rpc_error(output: &Output) -> Value {
    let response: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "decode CLI error JSON: {error}; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    response["error"].clone()
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
