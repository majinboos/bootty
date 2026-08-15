#![cfg(target_os = "macos")]

use std::{
    env,
    path::Path,
    process::{Command, Output},
};

use bootty_cli::align_shell_env;

const CHILD_MODE: &str = "BOOTTY_SHELL_CONTRACT_CHILD";
const ALIGN_MODE: &str = "align";

#[test]
fn align_shell_env_advertises_bootty_shell_in_a_fresh_process() {
    let directory = tempfile::tempdir().expect("temporary shell directory");
    let shell = directory.path().join("login-shell");
    let output = run_child(ALIGN_MODE, &shell);

    assert!(output.status.success(), "child failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!(
            "BOOTTY_SHELL_CONTRACT_SHELL={}\n",
            shell.display()
        )),
        "child output did not contain the advertised shell: {}",
        stdout
    );
}

#[test]
fn shell_environment_child() {
    if let Ok(ALIGN_MODE) = env::var(CHILD_MODE).as_deref() {
        align_shell_env();
        println!(
            "BOOTTY_SHELL_CONTRACT_SHELL={}",
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
        .expect("run isolated shell environment contract")
}
