use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{Result, bail};
use clap::Args as ClapArgs;
use serde::Serialize;
use tempfile::TempDir;

use crate::clock::{Timer, utc_datetime, utc_timestamp};

const REMOTE_PROBE: &str = r#"printf "remote shell ready\r\n"; i=0; while [ "$i" -lt 128 ]; do printf "\033[32mkey-%03d\033[0m echo\r\n" "$i"; i=$((i + 1)); done; i=0; while [ "$i" -lt 512 ]; do printf "remote log line %05d cargo/test/kubectl stream payload payload payload\r\n" "$i"; i=$((i + 1)); done; printf "\033[8;40;120tremote resize ack 120x40\r\n"; printf "\033]8;id=remote;https://example.invalid/remote\033\\link\033]8;;\033\\\r\n""#;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// JSONL output path.
    pub output_file: Option<PathBuf>,
}

#[derive(Serialize)]
struct Metadata {
    schema_version: u8,
    event: &'static str,
    recorded_at_utc: String,
    uname: String,
    shell: String,
}

#[derive(Serialize)]
struct Probe<'a> {
    schema_version: u8,
    event: &'static str,
    name: &'a str,
    profile: &'a str,
    status: &'a str,
    detail: String,
    duration_ns: u128,
    bytes: u64,
    exit_code: i32,
}

pub fn run(args: Args) -> Result<()> {
    crate::cancellation::install()?;
    let output_file = match args.output_file {
        Some(path) => path,
        None => PathBuf::from("artifacts/live-remote")
            .join(format!("live-remote-{}.jsonl", utc_timestamp()?)),
    };
    if let Some(parent) = output_file.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut output = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&output_file)?;
    let work = tempfile::Builder::new()
        .prefix("bootty-live-remote.")
        .tempdir()?;
    write_json(
        &mut output,
        &Metadata {
            schema_version: 1,
            event: "metadata",
            recorded_at_utc: utc_datetime()?,
            uname: uname(),
            shell: std::env::var("SHELL").unwrap_or_else(|_| "unknown".into()),
        },
    )?;

    if program_exists("ssh") && ssh_localhost_available()? {
        run_probe(
            &mut output,
            &work,
            "localhost_ssh",
            "localhost",
            ssh("localhost", 2),
        )?;
    } else {
        skip(
            &mut output,
            "localhost_ssh",
            "localhost",
            "ssh localhost is unavailable",
        )?;
    }

    match std::env::var("BOOTTY_LIVE_SSH_TARGET")
        .ok()
        .filter(|v| !v.is_empty())
    {
        Some(target) if program_exists("ssh") => {
            run_probe(&mut output, &work, "lan_ssh", "lan", ssh(&target, 5))?;
            for (name, profile, delay, loss) in [
                ("wan_20ms_ssh", "wan_20ms", "20ms", "0%"),
                ("wan_100ms_ssh", "wan_100ms", "100ms", "0.1%"),
                ("wan_200ms_ssh", "wan_200ms", "200ms", "1%"),
            ] {
                with_netem(
                    &mut output,
                    &work,
                    name,
                    profile,
                    delay,
                    loss,
                    ssh(&target, 5),
                )?;
            }
        }
        Some(_) => skip(&mut output, "lan_ssh", "lan", "ssh not found")?,
        None => skip(
            &mut output,
            "lan_ssh",
            "lan",
            "BOOTTY_LIVE_SSH_TARGET is not set",
        )?,
    }

    optional_probe(
        &mut output,
        &work,
        "mosh",
        "mosh",
        "BOOTTY_LIVE_MOSH_TARGET",
        |target| {
            let mut command = Command::new("mosh");
            command.args([target, "--", "sh", "-lc", REMOTE_PROBE]);
            command
        },
    )?;
    optional_probe(
        &mut output,
        &work,
        "docker_exec",
        "docker_exec",
        "BOOTTY_LIVE_DOCKER_CONTAINER",
        |target| {
            let mut command = Command::new("docker");
            command.args(["exec", target, "sh", "-lc", REMOTE_PROBE]);
            command
        },
    )?;
    optional_probe(
        &mut output,
        &work,
        "podman_exec",
        "podman_exec",
        "BOOTTY_LIVE_PODMAN_CONTAINER",
        |target| {
            let mut command = Command::new("podman");
            command.args(["exec", target, "sh", "-lc", REMOTE_PROBE]);
            command
        },
    )?;

    println!(
        "Wrote live remote benchmark results: {}",
        output_file.display()
    );
    Ok(())
}

fn optional_probe(
    output: &mut File,
    work: &TempDir,
    name: &str,
    program: &str,
    variable: &str,
    make_command: impl FnOnce(&str) -> Command,
) -> Result<()> {
    match std::env::var(variable).ok().filter(|v| !v.is_empty()) {
        Some(target) if program_exists(program) => {
            run_probe(output, work, name, name, make_command(&target))
        }
        Some(_) => skip(output, name, name, &format!("{program} not found")),
        None => skip(output, name, name, &format!("{variable} is not set")),
    }
}

fn ssh(target: &str, timeout: u8) -> Command {
    let mut command = Command::new("ssh");
    command.args([
        "-o",
        "BatchMode=yes",
        "-o",
        &format!("ConnectTimeout={timeout}"),
        target,
        "sh",
        "-lc",
        REMOTE_PROBE,
    ]);
    command
}

fn ssh_localhost_available() -> Result<bool> {
    let mut command = Command::new("ssh");
    command.args([
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=2",
        "localhost",
        "true",
    ]);
    match wait(&mut command, Duration::from_secs(5), None, None)? {
        Wait::Exited(status) => Ok(status.success()),
        Wait::Interrupted => Err(crate::cancellation::Interrupted.into()),
        Wait::TimedOut | Wait::StartFailed(_) => Ok(false),
    }
}

fn with_netem(
    output: &mut File,
    work: &TempDir,
    name: &str,
    profile: &str,
    delay: &str,
    loss: &str,
    command: Command,
) -> Result<()> {
    if std::env::var("BOOTTY_LIVE_NETEM_APPLY").as_deref() != Ok("1") {
        return skip(
            output,
            name,
            profile,
            "netem disabled; set BOOTTY_LIVE_NETEM_APPLY=1",
        );
    }
    let Some(interface) = std::env::var("BOOTTY_LIVE_NETEM_IFACE")
        .ok()
        .filter(|v| !v.is_empty())
    else {
        return skip(output, name, profile, "BOOTTY_LIVE_NETEM_IFACE is not set");
    };
    if !program_exists("tc") {
        return skip(output, name, profile, "tc not found");
    }

    let mut apply = Command::new("sudo");
    apply.args([
        "tc", "qdisc", "replace", "dev", &interface, "root", "netem", "delay", delay, "loss", loss,
    ]);
    if !apply.status()?.success() {
        bail!("failed to apply netem profile {profile}");
    }
    let guard = Netem { interface };
    let result = run_probe(output, work, name, profile, command);
    drop(guard);
    result
}

struct Netem {
    interface: String,
}

impl Drop for Netem {
    fn drop(&mut self) {
        let _ = Command::new("sudo")
            .args(["tc", "qdisc", "del", "dev", &self.interface, "root"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn run_probe(
    output: &mut File,
    work: &TempDir,
    name: &str,
    profile: &str,
    mut command: Command,
) -> Result<()> {
    let stdout_path = work.path().join(format!("{name}.stdout"));
    let stderr_path = work.path().join(format!("{name}.stderr"));
    let stdout = File::create(&stdout_path)?;
    let stderr = File::create(&stderr_path)?;
    let timer = Timer::start();
    let outcome = wait(
        &mut command,
        Duration::from_secs(20),
        Some(stdout),
        Some(stderr),
    )?;
    let duration_ns = timer.elapsed().as_nanos();
    let bytes = fs::metadata(&stdout_path)?.len();
    let (status, detail, exit_code) = match outcome {
        Wait::Exited(status) if status.success() => ("pass", "ok".into(), 0),
        Wait::Exited(status) => (
            "fail",
            first_line(&stderr_path),
            crate::cancellation::exit_code(status),
        ),
        Wait::TimedOut => ("fail", "timed out after 20s".into(), 124),
        Wait::StartFailed(detail) => ("fail", detail, 127),
        Wait::Interrupted => return Err(crate::cancellation::Interrupted.into()),
    };
    write_json(
        output,
        &Probe {
            schema_version: 1,
            event: "live_remote_probe",
            name,
            profile,
            status,
            detail,
            duration_ns,
            bytes,
            exit_code,
        },
    )
}

fn skip(output: &mut File, name: &str, profile: &str, detail: &str) -> Result<()> {
    write_json(
        output,
        &Probe {
            schema_version: 1,
            event: "live_remote_probe",
            name,
            profile,
            status: "skipped",
            detail: detail.into(),
            duration_ns: 0,
            bytes: 0,
            exit_code: 0,
        },
    )
}

enum Wait {
    Exited(ExitStatus),
    TimedOut,
    StartFailed(String),
    Interrupted,
}

fn wait(
    command: &mut Command,
    deadline: Duration,
    stdout: Option<File>,
    stderr: Option<File>,
) -> Result<Wait> {
    crate::process::configure_group(command);
    if let Some(stdout) = stdout {
        command.stdout(stdout);
    } else {
        command.stdout(Stdio::null());
    }
    if let Some(stderr) = stderr {
        command.stderr(stderr);
    } else {
        command.stderr(Stdio::null());
    }
    let mut child = match command.stdin(Stdio::null()).spawn() {
        Ok(child) => child,
        Err(error) => return Ok(Wait::StartFailed(error.to_string())),
    };
    let timer = Timer::start();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Wait::Exited(status));
        }
        if crate::cancellation::interrupted() {
            crate::process::terminate_group(&mut child);
            return Ok(Wait::Interrupted);
        }
        if timer.elapsed() >= deadline {
            crate::process::terminate_group(&mut child);
            return Ok(Wait::TimedOut);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn program_exists(program: impl AsRef<OsStr>) -> bool {
    let program = Path::new(program.as_ref());
    if program.components().count() > 1 {
        return program.is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

fn uname() -> String {
    Command::new("uname")
        .arg("-a")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map_or_else(
            || "unknown".into(),
            |o| String::from_utf8_lossy(&o.stdout).trim_end().to_owned(),
        )
}

fn first_line(path: &Path) -> String {
    let mut text = String::new();
    File::open(path)
        .and_then(|mut file| file.read_to_string(&mut text))
        .ok();
    text.lines().next().unwrap_or("command failed").to_owned()
}

fn write_json(output: &mut File, value: &impl Serialize) -> Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    writeln!(output)?;
    Ok(())
}
