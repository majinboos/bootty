use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use crate::command::run as run_command;
use crate::filesystem::{copy_file, recreate_dir};

const PACKAGE: &str = "bootty-daemon";
const PROFILE: &str = "daemon-release";
pub const MAX_DAEMON_BYTES: u64 = 13_631_488;

pub const TARGETS: [DaemonTarget; 5] = [
    DaemonTarget::Aarch64AppleDarwin,
    DaemonTarget::X86_64AppleDarwin,
    DaemonTarget::X86_64UnknownLinuxGnu,
    DaemonTarget::Aarch64UnknownLinuxGnu,
    DaemonTarget::X86_64PcWindowsMsvc,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum DaemonTarget {
    #[value(name = "aarch64-apple-darwin")]
    Aarch64AppleDarwin,
    #[value(name = "x86_64-apple-darwin")]
    X86_64AppleDarwin,
    #[value(name = "x86_64-unknown-linux-gnu")]
    X86_64UnknownLinuxGnu,
    #[value(name = "aarch64-unknown-linux-gnu")]
    Aarch64UnknownLinuxGnu,
    #[value(name = "x86_64-pc-windows-msvc")]
    X86_64PcWindowsMsvc,
}

impl DaemonTarget {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Aarch64AppleDarwin => "aarch64-apple-darwin",
            Self::X86_64AppleDarwin => "x86_64-apple-darwin",
            Self::X86_64UnknownLinuxGnu => "x86_64-unknown-linux-gnu",
            Self::Aarch64UnknownLinuxGnu => "aarch64-unknown-linux-gnu",
            Self::X86_64PcWindowsMsvc => "x86_64-pc-windows-msvc",
        }
    }

    pub const fn binary_name(self) -> &'static str {
        match self {
            Self::X86_64PcWindowsMsvc => "bootty-daemon.exe",
            _ => "bootty-daemon",
        }
    }

    pub fn artifact_name(self) -> String {
        format!("bootty-daemon-{self}")
    }

    const fn runner(self) -> &'static str {
        match self {
            Self::Aarch64AppleDarwin => "macos-26",
            Self::X86_64AppleDarwin => "macos-15-intel",
            Self::X86_64PcWindowsMsvc => "windows-2025",
            Self::X86_64UnknownLinuxGnu | Self::Aarch64UnknownLinuxGnu => "ubuntu-24.04",
        }
    }
}

impl fmt::Display for DaemonTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Args, Debug)]
pub struct DaemonArgs {
    #[command(subcommand)]
    pub command: DaemonCommand,
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Build and stage every supported daemon target.
    BuildAll,
    /// Print the GitHub Actions target matrix as JSON.
    Matrix,
    /// Build one target using the native CI policy.
    BuildOne { target: DaemonTarget },
    /// Copy one built daemon into a normalized artifact directory.
    Stage {
        target: DaemonTarget,
        output_dir: PathBuf,
    },
    /// Check that a directory contains every supported daemon artifact.
    Verify { output_dir: PathBuf },
    /// Enforce the daemon artifact size budget.
    Size { artifact: PathBuf },
}

pub fn run(args: &DaemonArgs) -> Result<()> {
    match &args.command {
        DaemonCommand::BuildAll => build_all(),
        DaemonCommand::Matrix => {
            println!("{}", matrix_json()?);
            Ok(())
        }
        DaemonCommand::BuildOne { target } => build_one(*target),
        DaemonCommand::Stage { target, output_dir } => stage(*target, output_dir),
        DaemonCommand::Verify { output_dir } => verify(output_dir),
        DaemonCommand::Size { artifact } => check_size(artifact),
    }
}

pub fn build_all() -> Result<()> {
    if let Some(output_dir) = env::var_os("BOOTTY_DAEMON_OUTPUT_DIR") {
        if output_dir.is_empty() {
            bail!("BOOTTY_DAEMON_OUTPUT_DIR must name a complete staged daemon directory");
        }
        let output_dir = PathBuf::from(output_dir);
        verify(&output_dir)?;
        return print_outputs(&output_dir);
    }

    let output_dir = target_root().join("bootty-daemons");
    let mut rustup = Command::new("rustup");
    rustup.args(["target", "add"]);
    rustup.args(TARGETS.map(DaemonTarget::as_str));
    run_command(&mut rustup)?;

    recreate_dir(&output_dir)?;
    for target in TARGETS {
        build_local(target)?;
        stage(target, &output_dir)?;
    }
    verify(&output_dir)?;
    print_outputs(&output_dir)
}

/// Build one daemon on its CI runner. The runner matrix guarantees that native
/// Apple, Windows, and `x86_64` Linux builds do not need cross-build tooling.
pub fn build_one(target: DaemonTarget) -> Result<()> {
    let mut rustup = Command::new("rustup");
    rustup.args(["target", "add", target.as_str()]);
    run_command(&mut rustup)?;

    let program = if target == DaemonTarget::Aarch64UnknownLinuxGnu {
        "zigbuild"
    } else {
        "build"
    };
    cargo(program, target)
}

pub fn stage(target: DaemonTarget, output_dir: &Path) -> Result<()> {
    let source = target_root()
        .join(target.as_str())
        .join(PROFILE)
        .join(target.binary_name());
    copy_file(&source, &output_dir.join(target.artifact_name()))
}

pub fn verify(output_dir: &Path) -> Result<()> {
    for target in TARGETS {
        let artifact = output_dir.join(target.artifact_name());
        let metadata = fs::metadata(&artifact)
            .with_context(|| format!("missing daemon artifact: {}", artifact.display()))?;
        if !metadata.is_file() || metadata.len() == 0 {
            bail!("missing daemon artifact: {}", artifact.display());
        }
    }
    Ok(())
}

pub fn check_size(artifact: &Path) -> Result<()> {
    let metadata = fs::metadata(artifact)
        .with_context(|| format!("missing daemon artifact: {}", artifact.display()))?;
    if !metadata.is_file() || metadata.len() == 0 {
        bail!("missing daemon artifact: {}", artifact.display());
    }
    let size = metadata.len();
    println!("bootty-daemon size: {size} bytes");
    if size > MAX_DAEMON_BYTES {
        bail!("daemon artifact exceeds {MAX_DAEMON_BYTES} byte budget: {size} bytes");
    }
    Ok(())
}

pub fn matrix_json() -> Result<String> {
    #[derive(Serialize)]
    struct Entry {
        target: &'static str,
        runner: &'static str,
    }

    #[derive(Serialize)]
    struct Matrix {
        include: Vec<Entry>,
    }

    let include = TARGETS
        .iter()
        .map(|target| Entry {
            target: target.as_str(),
            runner: target.runner(),
        })
        .collect();
    serde_json::to_string(&Matrix { include }).context("failed to serialize daemon target matrix")
}

pub fn target_root() -> PathBuf {
    env::var_os("CARGO_TARGET_DIR").map_or_else(|| PathBuf::from("target"), PathBuf::from)
}

fn build_local(target: DaemonTarget) -> Result<()> {
    match target {
        DaemonTarget::Aarch64AppleDarwin | DaemonTarget::X86_64AppleDarwin => {
            if cfg!(target_os = "macos") {
                cargo("build", target)
            } else if env::var_os("SDKROOT").is_some_and(|value| !value.is_empty()) {
                cargo("zigbuild", target)
            } else {
                bail!("SDKROOT is required to cross-build {target} outside Darwin")
            }
        }
        DaemonTarget::X86_64UnknownLinuxGnu | DaemonTarget::Aarch64UnknownLinuxGnu => {
            cargo("zigbuild", target)
        }
        DaemonTarget::X86_64PcWindowsMsvc => cargo("xwin", target),
    }
}

fn cargo(subcommand: &str, target: DaemonTarget) -> Result<()> {
    let mut command = Command::new("cargo");
    command.env("RUSTFLAGS", "");
    command.arg(subcommand);
    if subcommand == "xwin" {
        command.arg("build");
    }
    command.args(["--profile", PROFILE, "-p", PACKAGE, "--target"]);
    command.arg(target.as_str());
    run_command(&mut command)
}

fn print_outputs(output_dir: &Path) -> Result<()> {
    let mut outputs = fs::read_dir(output_dir)
        .with_context(|| format!("failed to read {}", output_dir.display()))?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(std::fs::FileType::is_file)
                .map(|_| entry.path())
        })
        .collect::<Vec<_>>();
    outputs.sort();
    for output in outputs {
        println!("{}", output.display());
    }
    Ok(())
}
