#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::process::Command;

use assert_fs::TempDir;
use assert_fs::prelude::*;
use pretty_assertions::assert_eq;
use serde_json::Value;

#[test]
fn hostile_soak_records_all_three_cases() {
    let temp = TempDir::new().unwrap();
    let bin = temp.child("bin");
    bin.create_dir_all().unwrap();
    symlink("/usr/bin/true", bin.child("cargo").path()).unwrap();
    let output = temp.child("output");

    let status = Command::new(env!("CARGO_BIN_EXE_xtasks"))
        .args(["benchmark", "hostile-soak"])
        .arg(output.path())
        .env_clear()
        .env("PATH", bin.path())
        .status()
        .unwrap();

    assert!(status.success());
    let records = fs::read_to_string(output.child("summary.jsonl").path())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0]["name"], "hostile_mixed_soak_256_rounds");
    assert_eq!(records[1]["name"], "hostile_extended_recovery_ladder");
    assert_eq!(records[2]["name"], "hostile_long_line_16mb_write");
    assert!(records.iter().all(|record| record["status"] == "pass"));
}
