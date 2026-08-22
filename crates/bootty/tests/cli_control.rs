#![cfg(unix)]

use std::{
    fs,
    process::{Command, Output, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use bootty_app::{
    AppState, FrameInputs, ViewportSnapshot, commands::CommandCatalog, renderer::RendererMetrics,
};
use bootty_command::{
    AppCommandReceiver, AppCommandRequest, AppCommandSender, Caller, CommandOutcome,
    app_command_channel as command_channel,
};
use bootty_config::config::{BoottyConfig, MultiplexerBackendConfig};
use bootty_control::{ControlPlane, ControlServer};
use bootty_extension::ExtensionHost;
use bootty_render::geometry::ViewTransform;
use serde_json::{Value, json};

mod support;

const HELPER_ENV: &str = "BOOTTY_CLI_CONTROL_TEST_HELPER";

fn app_command_channel(capacity: usize) -> (AppCommandSender, AppCommandReceiver) {
    command_channel(capacity, Arc::new(|| {}))
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
    let control_catalog = catalog.control_catalog();
    let server = ControlServer::spawn(
        "main",
        sender.for_caller(Caller::Socket),
        Arc::clone(&control_catalog),
        &control_plane,
    )
    .expect("start control owner");

    let mut child = spawn_bootty(&["command", "ignore"]);
    let request = receive_request(&receiver);
    assert_eq!(request.invocation.command, "ignore");
    assert_eq!(request.invocation.arguments, Vec::<String>::new());
    assert_eq!(request.invocation.caller, Caller::Socket);
    request
        .response
        .send(CommandOutcome::success())
        .expect("complete CLI command");
    assert!(child.wait().expect("wait for CLI command").success());

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
        multiplexer: bootty_config::config::MultiplexerConfig {
            backend: MultiplexerBackendConfig::Native,
            ..bootty_config::config::MultiplexerConfig::default()
        },
        ..BoottyConfig::default()
    };
    let mut state = AppState::new(config, support::backends(), Arc::new(|| {}), None, None)
        .expect("native app state");
    let native_control_plane = ControlPlane::default();
    let mut extension_host = ExtensionHost::load(
        &extension_root,
        state.command_catalog().extensions_arc(),
        state.app_command_sender(Caller::Luau),
        native_control_plane.extension_event_sender(),
    );
    let native_catalog = state.command_catalog().control_catalog();
    let native_server = ControlServer::spawn(
        "main",
        state.app_command_sender(Caller::Socket),
        native_catalog,
        &native_control_plane,
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

fn receive_request(receiver: &AppCommandReceiver) -> AppCommandRequest {
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
