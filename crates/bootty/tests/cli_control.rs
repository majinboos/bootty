#![cfg(unix)]

use pretty_assertions::assert_eq;

use std::{
    process::{Command, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use bootty_app::commands::CommandCatalog;
use bootty_command::{
    AppCommandReceiver, AppCommandRequest, AppCommandSender, Caller, CommandOutcome,
    app_command_channel as command_channel,
};
use bootty_control::{ControlPlane, ControlServer};

const HELPER_ENV: &str = "BOOTTY_CLI_CONTROL_TEST_HELPER";

fn app_command_channel(capacity: usize) -> (AppCommandSender, AppCommandReceiver) {
    command_channel(capacity, Arc::new(|| {}))
}

#[test]
fn one_executable_uses_the_live_owner_without_a_second_gui() {
    let runtime = assert_fs::TempDir::new().expect("temporary runtime directory");
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
