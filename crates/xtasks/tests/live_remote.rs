use std::fs;
use std::process::Command;

use assert_fs::TempDir;
use assert_fs::prelude::*;
use pretty_assertions::assert_eq;
use serde_json::Value;

#[test]
fn live_remote_records_unavailable_probes_as_skipped() {
    let temp = TempDir::new().unwrap();
    let output = temp.child("results/live.jsonl");

    let status = Command::new(env!("CARGO_BIN_EXE_xtasks"))
        .args(["benchmark", "live-remote"])
        .arg(output.path())
        .env_clear()
        .env("PATH", temp.path())
        .status()
        .unwrap();

    assert!(status.success());
    let records = fs::read_to_string(output.path())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 6);
    assert_eq!(records[0]["event"], "metadata");
    assert!(
        records[1..]
            .iter()
            .all(|record| record["status"] == "skipped")
    );
}
