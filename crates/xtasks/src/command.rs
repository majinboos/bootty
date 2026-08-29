use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::process::Stdio;
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};

pub fn run(command: &mut Command) -> Result<()> {
    let display = display(command);
    let status = command
        .status()
        .with_context(|| format!("failed to start {display}"))?;
    if !status.success() {
        bail!("{display} exited with {status}");
    }
    Ok(())
}

pub fn output(command: &mut Command) -> Result<Output> {
    let display = display(command);
    let output = command
        .output()
        .with_context(|| format!("failed to start {display}"))?;
    if !output.status.success() {
        bail!("{display} exited with {}", output.status);
    }
    Ok(output)
}

pub fn stdout(command: &mut Command) -> Result<String> {
    let output = output(command)?;
    String::from_utf8(output.stdout).context("command output was not UTF-8")
}

#[cfg(unix)]
pub fn program_exists(program: impl AsRef<OsStr>) -> bool {
    Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

pub fn append_env(name: &str, suffix: impl AsRef<OsStr>) -> OsString {
    let mut value = std::env::var_os(name).unwrap_or_default();
    if !value.is_empty() {
        value.push(" ");
    }
    value.push(suffix);
    value
}

pub fn display(command: &Command) -> String {
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}
