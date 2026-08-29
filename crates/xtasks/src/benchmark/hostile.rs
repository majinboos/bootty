use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Args as ClapArgs;
use serde::Serialize;

use crate::clock::{Timer, utc_timestamp};

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Directory to receive logs and summary.jsonl.
    pub output_dir: Option<PathBuf>,
}

#[derive(Serialize)]
struct Record<'a> {
    schema_version: u8,
    event: &'static str,
    name: &'a str,
    status: &'static str,
    detail: String,
    duration_s: u64,
    exit_code: i32,
    log: String,
}

pub fn run(args: Args) -> Result<()> {
    crate::cancellation::install()?;
    let output_dir = match args.output_dir {
        Some(path) => path,
        None => PathBuf::from("artifacts/hostile-soak").join(utc_timestamp()?),
    };
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;
    let mut summary = File::create(output_dir.join("summary.jsonl"))?;
    let timeout = environment_u64("BOOTTY_HOSTILE_SOAK_SECONDS", 10)?;
    let sample_size = environment_u64("BOOTTY_HOSTILE_SOAK_SAMPLE_SIZE", 10)?;

    for name in [
        "hostile_mixed_soak_256_rounds",
        "hostile_extended_recovery_ladder",
        "hostile_long_line_16mb_write",
    ] {
        run_case(&output_dir, &mut summary, name, timeout, sample_size)?;
    }

    println!("Wrote hostile soak evidence: {}", output_dir.display());
    Ok(())
}

fn environment_u64(name: &str, default: u64) -> Result<u64> {
    match std::env::var(name) {
        Ok(value) => value
            .parse()
            .with_context(|| format!("{name} must be a non-negative integer")),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error).with_context(|| format!("failed to read {name}")),
    }
}

fn run_case(
    output_dir: &std::path::Path,
    summary: &mut File,
    name: &str,
    timeout_seconds: u64,
    sample_size: u64,
) -> Result<()> {
    let log_path = output_dir.join(format!("{name}.log"));
    let log = File::create(&log_path)?;
    let mut command = Command::new("cargo");
    command.args([
        "bench",
        "-p",
        "bootty-app",
        "--bench",
        "hostile_input",
        name,
        "--",
        "--sample-size",
        &sample_size.to_string(),
        "--measurement-time",
        "0.2",
        "--warm-up-time",
        "0.1",
    ]);
    command.stdout(log.try_clone()?).stderr(log);

    let timer = Timer::start();
    let outcome = wait_with_deadline(&mut command, Duration::from_secs(timeout_seconds))?;
    let (status, detail, exit_code) = match outcome {
        Outcome::Exited(status) if status.success() => ("pass", "ok".into(), 0),
        Outcome::Exited(status) => (
            "fail",
            last_line(&log_path).unwrap_or_else(|| "command failed".into()),
            crate::cancellation::exit_code(status),
        ),
        Outcome::TimedOut => (
            "timeout",
            format!("timed out after {timeout_seconds}s"),
            124,
        ),
        Outcome::StartFailed(detail) => ("fail", detail, 127),
        Outcome::Interrupted => return Err(crate::cancellation::Interrupted.into()),
    };
    serde_json::to_writer(
        &mut *summary,
        &Record {
            schema_version: 1,
            event: "hostile_soak",
            name,
            status,
            detail,
            duration_s: timer.elapsed().as_secs(),
            exit_code,
            log: log_path.display().to_string(),
        },
    )?;
    writeln!(summary)?;
    Ok(())
}

enum Outcome {
    Exited(ExitStatus),
    TimedOut,
    StartFailed(String),
    Interrupted,
}

fn wait_with_deadline(command: &mut Command, deadline: Duration) -> Result<Outcome> {
    crate::process::configure_group(command);
    let mut child = match command.stdin(Stdio::null()).spawn() {
        Ok(child) => child,
        Err(error) => return Ok(Outcome::StartFailed(error.to_string())),
    };
    let timer = Timer::start();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Outcome::Exited(status));
        }
        if crate::cancellation::interrupted() {
            crate::process::terminate_group(&mut child);
            return Ok(Outcome::Interrupted);
        }
        if timer.elapsed() >= deadline {
            crate::process::terminate_group(&mut child);
            return Ok(Outcome::TimedOut);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn last_line(path: &std::path::Path) -> Option<String> {
    BufReader::new(File::open(path).ok()?)
        .lines()
        .map_while(Result::ok)
        .last()
}
