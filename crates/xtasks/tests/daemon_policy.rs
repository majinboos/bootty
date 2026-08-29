use std::fs::{self, File};

use assert_fs::TempDir;
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::json;
use xtasks::daemon::{DaemonTarget, MAX_DAEMON_BYTES, TARGETS, check_size, matrix_json, verify};

#[rstest]
#[case(
    DaemonTarget::Aarch64AppleDarwin,
    "aarch64-apple-darwin",
    "bootty-daemon"
)]
#[case(
    DaemonTarget::X86_64AppleDarwin,
    "x86_64-apple-darwin",
    "bootty-daemon"
)]
#[case(
    DaemonTarget::X86_64UnknownLinuxGnu,
    "x86_64-unknown-linux-gnu",
    "bootty-daemon"
)]
#[case(
    DaemonTarget::Aarch64UnknownLinuxGnu,
    "aarch64-unknown-linux-gnu",
    "bootty-daemon"
)]
#[case(
    DaemonTarget::X86_64PcWindowsMsvc,
    "x86_64-pc-windows-msvc",
    "bootty-daemon.exe"
)]
fn target_names_are_the_artifact_contract(
    #[case] target: DaemonTarget,
    #[case] triple: &str,
    #[case] binary: &str,
) {
    assert_eq!(target.as_str(), triple);
    assert_eq!(target.binary_name(), binary);
    assert_eq!(target.artifact_name(), format!("bootty-daemon-{triple}"));
}

#[rstest]
fn matrix_keeps_target_and_runner_policy_together() {
    let matrix: serde_json::Value = serde_json::from_str(&matrix_json().unwrap()).unwrap();

    assert_eq!(
        matrix,
        json!({"include": [
            {"target": "aarch64-apple-darwin", "runner": "macos-26"},
            {"target": "x86_64-apple-darwin", "runner": "macos-15-intel"},
            {"target": "x86_64-unknown-linux-gnu", "runner": "ubuntu-24.04"},
            {"target": "aarch64-unknown-linux-gnu", "runner": "ubuntu-24.04"},
            {"target": "x86_64-pc-windows-msvc", "runner": "windows-2025"}
        ]})
    );
}

#[rstest]
fn verify_requires_every_nonempty_artifact() {
    let output = TempDir::new().unwrap();
    for target in TARGETS {
        fs::write(output.path().join(target.artifact_name()), b"daemon").unwrap();
    }
    verify(output.path()).unwrap();

    fs::write(
        output
            .path()
            .join(DaemonTarget::Aarch64UnknownLinuxGnu.artifact_name()),
        [],
    )
    .unwrap();
    assert!(verify(output.path()).is_err());
}

#[rstest]
fn size_budget_rejects_oversized_artifacts_without_writing_large_fixtures() {
    let output = TempDir::new().unwrap();
    let artifact = output.path().join("bootty-daemon");
    let file = File::create(&artifact).unwrap();
    file.set_len(MAX_DAEMON_BYTES).unwrap();
    check_size(&artifact).unwrap();

    file.set_len(MAX_DAEMON_BYTES + 1).unwrap();
    assert!(check_size(&artifact).is_err());
}
