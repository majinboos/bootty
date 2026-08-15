use std::{process::Command, thread, time::Duration};

use bootty_mux::process::{
    CancellableCommandRunner, CommandCancellation, CommandRunner, SystemCommandRunner,
};

const HELPER_ENV: &str = "BOOTTY_MUX_PROCESS_CONTRACT_HELPER";

#[cfg(target_os = "macos")]
#[test]
fn disowned_commands_resolve_programs_and_preserve_the_bootty_environment() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("tempdir");
    let program = directory.path().join("bootty-env-probe");
    let captured_path = directory.path().join("captured-path");
    let captured_custom = directory.path().join("captured-custom");
    std::fs::write(
        &program,
        "#!/bin/sh\nprintf '%s' \"$PATH\" > \"$1\"\nprintf '%s' \"$BOOTTY_ENV_PROBE\" > \"$2\"",
    )
    .expect("write environment probe");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
        .expect("make environment probe executable");
    let path = std::env::join_paths(std::iter::once(directory.path().to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("join PATH");

    let output = Command::new(std::env::current_exe().expect("current test executable"))
        .args(["--exact", "process_contract_helper", "--nocapture"])
        .env(HELPER_ENV, "1")
        .env("PATH", path)
        .env("BOOTTY_ENV_PROBE", "login-env-value")
        .env("BOOTTY_ENV_PROBE_PROGRAM", &program)
        .env("BOOTTY_ENV_PROBE_PATH", &captured_path)
        .env("BOOTTY_ENV_PROBE_CUSTOM", &captured_custom)
        .output()
        .expect("run isolated process contract");

    assert!(
        output.status.success(),
        "stdout={}; stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let child_path = std::fs::read_to_string(captured_path).expect("captured PATH");
    assert!(
        std::env::split_paths(std::ffi::OsStr::new(&child_path))
            .any(|entry| entry == directory.path())
    );
    assert_eq!(
        std::fs::read_to_string(captured_custom).expect("captured custom environment"),
        "login-env-value"
    );
}

#[test]
fn process_contract_helper() {
    if std::env::var_os(HELPER_ENV).is_none() {
        return;
    }

    #[cfg(target_os = "macos")]
    {
        let program = std::env::var("BOOTTY_ENV_PROBE_PROGRAM").expect("probe program");
        let captured_path = std::env::var("BOOTTY_ENV_PROBE_PATH").expect("captured PATH");
        let captured_custom =
            std::env::var("BOOTTY_ENV_PROBE_CUSTOM").expect("captured custom environment");
        let resolved = bootty_mux::process::resolve_program("bootty-env-probe")
            .expect("resolve bare program through PATH");
        assert_eq!(resolved, program);
        assert_eq!(
            bootty_mux::process::resolve_program("./tmux").expect("keep relative program"),
            "./tmux"
        );

        let output = SystemCommandRunner
            .run_disowned("bootty-env-probe", &[captured_path, captured_custom])
            .expect("run disowned environment probe");
        assert!(output.success, "probe failed: {}", output.stderr);
    }
}

#[test]
fn canceled_runner_does_not_start_a_command() {
    let cancellation = CommandCancellation::default();
    cancellation.cancel();
    let runner = CancellableCommandRunner::new(cancellation);

    assert!(
        runner
            .run("bootty-command-that-must-not-exist", &[])
            .unwrap_err()
            .to_string()
            .contains("canceled")
    );
}

#[cfg(unix)]
#[test]
fn cancellation_stops_a_running_command() {
    let cancellation = CommandCancellation::default();
    let runner = CancellableCommandRunner::new(cancellation.clone());
    let worker = thread::spawn(move || runner.run("sh", &["-c".to_owned(), "sleep 10".to_owned()]));
    thread::sleep(Duration::from_millis(50));

    cancellation.cancel();
    let error = worker
        .join()
        .expect("command worker")
        .expect_err("canceled command");

    assert!(error.to_string().contains("canceled"));
}
