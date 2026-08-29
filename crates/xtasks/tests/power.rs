#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use assert_fs::TempDir;
use assert_fs::prelude::*;
use pretty_assertions::assert_eq;
use serde_json::Value;

#[test]
fn power_preserves_measured_command_failure_in_evidence() {
    let temp = TempDir::new().unwrap();
    let output = temp.child("power");

    let status = Command::new(env!("CARGO_BIN_EXE_xtasks"))
        .args(["benchmark", "power-thermal"])
        .arg(output.path())
        .args(["--", "/bin/sh", "-c", "exit 7"])
        .env_clear()
        .env("PATH", temp.path())
        .status()
        .unwrap();

    // The top-level CLI maps the typed command failure back to its original code.
    assert_eq!(status.code(), Some(7));
    let records = fs::read_to_string(output.child("summary.jsonl").path())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 3);
    assert_eq!(records[1]["status"], "skipped");
    assert_eq!(records[2]["status"], "fail");
    assert_eq!(records[2]["exit_code"], 7);
}

#[test]
fn power_stops_continuous_nvidia_sampler_at_the_deadline() {
    let temp = TempDir::new().unwrap();
    let bin = temp.child("bin");
    bin.create_dir_all().unwrap();
    let nvidia_smi = bin.child("nvidia-smi");
    fs::write(nvidia_smi.path(), "#!/bin/sh\nwhile :; do :; done\n").unwrap();
    let mut permissions = fs::metadata(nvidia_smi.path()).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(nvidia_smi.path(), permissions).unwrap();
    let output = temp.child("power");

    let started = Instant::now();
    let status = Command::new(env!("CARGO_BIN_EXE_xtasks"))
        .args(["benchmark", "power-thermal"])
        .arg(output.path())
        .args(["--", "/usr/bin/true"])
        .env_clear()
        .env("PATH", bin.path())
        .env("BOOTTY_POWER_SECONDS", "0")
        .env("BOOTTY_POWER_INTERVAL_MS", "1")
        .status()
        .unwrap();

    assert!(status.success());
    assert!(started.elapsed() < Duration::from_secs(3));
    let records = fs::read_to_string(output.child("summary.jsonl").path())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records[1]["tool"], "nvidia-smi");
    assert_eq!(records[2]["status"], "pass");
}

#[test]
fn power_terminates_the_measured_process_group_on_sigterm() {
    let temp = TempDir::new().unwrap();
    let descendant_pid = temp.child("descendant.pid");
    let output = temp.child("power");
    let command = format!(
        "trap '' TERM; (trap '' TERM; while :; do sleep 1; done) & echo $! > '{}'; wait",
        descendant_pid.path().display()
    );
    let mut xtasks = Command::new(env!("CARGO_BIN_EXE_xtasks"))
        .args(["benchmark", "power-thermal"])
        .arg(output.path())
        .args(["--", "/bin/sh", "-c", &command])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    for _ in 0..100 {
        if descendant_pid.path().is_file() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(descendant_pid.path().is_file());
    assert!(
        Command::new("/bin/kill")
            .args(["-TERM", &xtasks.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(xtasks.wait().unwrap().code(), Some(130));

    let pid = fs::read_to_string(descendant_pid.path()).unwrap();
    assert!(
        !Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success(),
        "descendant {pid} survived xtasks cancellation"
    );
}
