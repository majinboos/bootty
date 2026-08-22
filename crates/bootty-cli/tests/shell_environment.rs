#![cfg(target_os = "macos")]

use std::{
    env,
    path::Path,
    process::{Command, Output},
};

use assert_fs::{TempDir, fixture::PathChild};
use bootty_cli::align_shell_env;
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};

const CHILD_MODE: &str = "BOOTTY_SHELL_TEST_CHILD";
const ALIGN_MODE: &str = "align";
const SHELL_OUTPUT_PREFIX: &str = "BOOTTY_SHELL_TEST_VALUE=";

#[fixture]
fn shell_dir() -> TempDir {
    TempDir::new().expect("temporary shell directory")
}

#[rstest]
fn align_shell_env_advertises_the_configured_shell(shell_dir: TempDir) {
    let shell = shell_dir.child("login-shell");
    let output = run_child(ALIGN_MODE, shell.path());
    let stdout = String::from_utf8(output.stdout).expect("child output is UTF-8");
    let advertised = stdout
        .lines()
        .find(|line| line.starts_with(SHELL_OUTPUT_PREFIX))
        .map(str::to_owned);

    assert_eq!(
        (output.status.code(), advertised),
        (
            Some(0),
            Some(format!("{SHELL_OUTPUT_PREFIX}{}", shell.path().display()))
        ),
        "child stdout: {stdout:?}; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn shell_environment_child() {
    if let Ok(ALIGN_MODE) = env::var(CHILD_MODE).as_deref() {
        align_shell_env();
        println!(
            "{SHELL_OUTPUT_PREFIX}{}",
            env::var("SHELL").expect("align_shell_env must set SHELL")
        );
    }
}

fn run_child(mode: &str, shell: &Path) -> Output {
    Command::new(env::current_exe().expect("test executable path"))
        .args(["--exact", "shell_environment_child", "--nocapture"])
        .env(CHILD_MODE, mode)
        .env("BOOTTY_SHELL", shell)
        .env("SHELL", "/bin/sh")
        .output()
        .expect("run isolated shell environment child")
}
