#![cfg(unix)]

use std::{
    process::{Command, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use bootty_app::{
    commands::{Caller, CommandCatalog, CommandOutcome, app_command_channel},
    control::{ControlPlane, ControlServer},
};

const HELPER_ENV: &str = "BOOTTY_CLI_CONTROL_TEST_HELPER";

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
    let server = ControlServer::spawn(
        "main".to_owned(),
        sender.for_caller(Caller::Socket),
        Arc::new(CommandCatalog::default()),
        ControlPlane::default(),
    )
    .expect("start control owner");

    let mut command = Command::new(env!("CARGO_BIN_EXE_bootty"));
    command.args(["command", "ignore"]);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start CLI command");
    let deadline = Instant::now() + Duration::from_secs(30);
    let request = loop {
        if let Ok(request) = receiver.try_recv() {
            break request;
        }
        if let Some(status) = child.try_wait().expect("poll CLI command") {
            panic!("CLI exited with {status} before reaching the app command channel");
        }
        assert!(
            Instant::now() < deadline,
            "CLI did not reach app command channel"
        );
        thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(request.invocation.command, "ignore");
    assert!(request.invocation.arguments.is_empty());
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
    let deadline = Instant::now() + Duration::from_secs(30);
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
