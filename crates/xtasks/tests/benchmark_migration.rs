#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;

use assert_fs::TempDir;
use assert_fs::prelude::*;
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::Value;

#[rstest]
fn suite_runs_every_ci_smoke_step_and_records_each_failure() {
    let temp = TempDir::new().expect("temp directory");
    let scripts = temp.child("scripts");
    scripts.create_dir_all().expect("scripts directory");
    executable(
        &scripts.child("validate-benchmark-manifests.py"),
        "#!/bin/sh\necho manifest-failure\nexit 9\n",
    );
    executable(
        &scripts.child("build-benchmark-dashboard.py"),
        "#!/bin/sh\necho dashboard-pass\n",
    );
    let bin = temp.child("bin");
    bin.create_dir_all().expect("bin directory");
    executable(
        &bin.child("cargo"),
        "#!/bin/sh\necho cargo-failure\nexit 17\n",
    );
    let output_dir = temp.child("evidence");

    let output = Command::new(env!("CARGO_BIN_EXE_xtasks"))
        .args(["benchmark", "suite", "--ci-smoke", "--output"])
        .arg(output_dir.path())
        .current_dir(temp.path())
        .env("PATH", path_with(bin.path()))
        .output()
        .expect("run benchmark suite");

    assert!(!output.status.success());
    let records = json_lines(&output_dir.child("summary.jsonl"));
    assert_eq!(records.len(), 4);
    assert_eq!(records[1]["name"], "validate_benchmark_manifests");
    assert_eq!(records[1]["status"], "fail");
    assert_eq!(records[1]["exit_code"], 9);
    assert_eq!(records[2]["name"], "validate_benchmark_dashboard");
    assert_eq!(records[2]["status"], "pass");
    assert_eq!(records[3]["name"], "compile_paint_plan");
    assert_eq!(records[3]["status"], "fail");
    assert_eq!(records[3]["exit_code"], 17);
    assert_eq!(
        fs::read_to_string(output_dir.child("compile_paint_plan.log").path()).expect("cargo log"),
        "cargo-failure\n"
    );
}

#[rstest]
fn recorder_passes_command_arguments_directly_and_writes_a_complete_bundle() {
    let temp = TempDir::new().expect("temp directory");
    let bin = temp.child("bin");
    bin.create_dir_all().expect("bin directory");
    executable(
        &bin.child("script"),
        r#"#!/bin/sh
if [ "$1" = "--help" ]; then
  echo 'BSD script help'
  exit 0
fi
stream=$2
shift 2
printf '%s\n' "$@" >"$stream"
"#,
    );
    let fixtures = temp.child("fixtures");

    let output = Command::new(env!("CARGO_BIN_EXE_xtasks"))
        .args([
            "benchmark",
            "record-replay",
            "shell-characters",
            fixtures.path().to_str().expect("UTF-8 fixture path"),
            "--",
            "printf",
            "two words",
            "$(not-executed)",
        ])
        .env("PATH", path_with(bin.path()))
        .env("COLUMNS", "132")
        .env("LINES", "43")
        .output()
        .expect("record replay fixture");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let fixture = fixtures.child("shell-characters");
    assert_eq!(
        fs::read_to_string(fixture.child("stream.pty").path()).expect("stream"),
        "printf\ntwo words\n$(not-executed)\n"
    );
    let timing = fs::read_to_string(fixture.child("timing.tsv").path()).expect("timing");
    assert!(timing.starts_with("start_ns\t"));
    assert!(timing.contains("\nend_ns\t"));
    assert!(timing.contains("\nduration_ns\t"));
    let metadata = fs::read_to_string(fixture.child("metadata.env").path()).expect("metadata");
    assert!(metadata.contains("cols=132\n"));
    assert!(metadata.contains("rows=43\n"));
    assert!(metadata.contains("'two words'"));
    assert!(metadata.contains("'$(not-executed)'"));
    let sums = fs::read_to_string(fixture.child("SHA256SUMS").path()).expect("checksums");
    assert_eq!(sums.lines().count(), 3);
    assert!(sums.contains("  stream.pty\n"));
    assert!(sums.contains("  timing.tsv\n"));
    assert!(sums.contains("  metadata.env\n"));
}

#[rstest]
#[case("../outside")]
#[case("nested/fixture")]
#[case("/tmp/outside")]
fn recorder_rejects_fixture_names_that_escape_the_output_root(#[case] name: &str) {
    let temp = TempDir::new().expect("temp directory");
    let output = Command::new(env!("CARGO_BIN_EXE_xtasks"))
        .args(["benchmark", "record-replay", name])
        .arg(temp.child("fixtures").path())
        .args(["--", "true"])
        .output()
        .expect("run replay recorder");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("one path component"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn json_lines(file: &assert_fs::fixture::ChildPath) -> Vec<Value> {
    fs::read_to_string(file.path())
        .expect("summary")
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSON record"))
        .collect()
}

fn path_with(first: &Path) -> std::ffi::OsString {
    let mut paths = vec![first.to_owned()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(paths).expect("joined PATH")
}

fn executable(path: &assert_fs::fixture::ChildPath, contents: &str) {
    path.write_str(contents).expect("write executable");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path.path()).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path.path(), permissions).expect("executable permissions");
    }
}
