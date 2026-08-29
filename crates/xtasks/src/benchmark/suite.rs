use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use serde::Serialize;

use crate::clock::{Timer, utc_datetime, utc_timestamp};
use crate::command;

const APP_BENCHMARKS: &[&str] = &[
    "paint_plan",
    "pipeline_resources",
    "paint_plan_wgpu",
    "startup_config",
    "startup_milestones",
    "kitty_image",
    "graphics_protocols",
    "app_frame",
    "text_atlas",
    "hostile_input",
    "panes_multiwindow",
    "multiplexer",
    "remote_session",
    "real_app_replay",
    "resize_reflow",
    "scrollback",
    "parser_control",
    "render_pacing",
    "input_latency",
    "idle_overhead",
    "power_thermal",
    "input_protocols",
];

#[derive(Clone, Debug, ClapArgs)]
pub struct Args {
    /// Run only the fast benchmark gate for blocking CI.
    #[arg(long)]
    ci_smoke: bool,

    /// Also run a small representative measured subset.
    #[arg(long)]
    quick: bool,

    /// Write logs and summary.jsonl under this directory.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Serialize)]
struct Metadata {
    schema_version: u8,
    event: &'static str,
    recorded_at_utc: String,
    commit: String,
    uname: String,
    rustc: String,
    cargo: String,
}

#[derive(Serialize)]
struct CommandRecord<'a> {
    schema_version: u8,
    event: &'static str,
    name: &'a str,
    status: &'static str,
    detail: String,
    duration_s: u64,
    exit_code: i32,
    command: String,
    log: String,
}

struct PlannedCommand {
    name: String,
    program: &'static str,
    args: Vec<String>,
}

impl PlannedCommand {
    fn new(
        name: impl Into<String>,
        program: &'static str,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            name: name.into(),
            program,
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(self.program);
        command.args(&self.args);
        command
    }
}

pub fn run(args: Args) -> Result<()> {
    let output_dir = match args.output {
        Some(path) => path,
        None => PathBuf::from("artifacts/benchmark-reproduction").join(utc_timestamp()?),
    };
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;
    let summary_path = output_dir.join("summary.jsonl");
    let mut summary = File::create(&summary_path)
        .with_context(|| format!("failed to create {}", summary_path.display()))?;
    write_json_line(&mut summary, &metadata()?)?;

    let mut failures = 0;
    for planned in plan(args.ci_smoke, args.quick) {
        if !run_logged(&planned, &output_dir, &mut summary)? {
            failures += 1;
        }
    }

    if failures == 0 {
        println!(
            "Wrote benchmark reproduction evidence: {}",
            output_dir.display()
        );
        return Ok(());
    }

    eprintln!(
        "Wrote benchmark reproduction evidence with {failures} failure(s): {}",
        output_dir.display()
    );
    bail!("benchmark reproduction failed")
}

fn metadata() -> Result<Metadata> {
    Ok(Metadata {
        schema_version: 1,
        event: "benchmark_reproduction_metadata",
        recorded_at_utc: utc_datetime()?,
        commit: command_stdout("git", &["rev-parse", "HEAD"]),
        uname: command_stdout("uname", &["-a"]),
        rustc: command_stdout("rustc", &["--version"]),
        cargo: command_stdout("cargo", &["--version"]),
    })
}

fn command_stdout(program: &str, args: &[&str]) -> String {
    let mut process = Command::new(program);
    process.args(args).stdin(Stdio::null());
    command::stdout(&mut process).map_or_else(
        |_| "unknown".to_owned(),
        |value| value.trim_end().to_owned(),
    )
}

fn plan(ci_smoke: bool, quick: bool) -> Vec<PlannedCommand> {
    let mut commands = vec![
        PlannedCommand::new(
            "validate_benchmark_manifests",
            "scripts/validate-benchmark-manifests.py",
            std::iter::empty::<String>(),
        ),
        PlannedCommand::new(
            "validate_benchmark_dashboard",
            "scripts/build-benchmark-dashboard.py",
            ["--self-test"],
        ),
    ];

    if ci_smoke {
        commands.push(PlannedCommand::new(
            "compile_paint_plan",
            "cargo",
            [
                "test",
                "-p",
                "bootty-app",
                "--bench",
                "paint_plan",
                "--no-run",
            ],
        ));
        return commands;
    }

    for benchmark in APP_BENCHMARKS {
        commands.push(PlannedCommand::new(
            format!("compile_{benchmark}"),
            "cargo",
            [
                "test",
                "-p",
                "bootty-app",
                "--bench",
                *benchmark,
                "--no-run",
            ],
        ));
    }
    commands.push(PlannedCommand::new(
        "compile_pty_drain",
        "cargo",
        [
            "test",
            "-p",
            "bootty-runtime",
            "--bench",
            "pty_drain",
            "--no-run",
        ],
    ));
    commands.push(PlannedCommand::new(
        "compile_flood_response",
        "cargo",
        [
            "test",
            "-p",
            "bootty-runtime",
            "--bench",
            "flood_response",
            "--no-run",
        ],
    ));

    if quick {
        commands.extend([
            PlannedCommand::new(
                "quick_paint_plan_smoke",
                "cargo",
                ["test", "-p", "bootty-app", "--bench", "paint_plan"],
            ),
            PlannedCommand::new(
                "quick_input_protocols",
                "cargo",
                [
                    "bench",
                    "-p",
                    "bootty-app",
                    "--bench",
                    "input_protocols",
                    "input_protocol_keyboard_legacy_printable",
                    "--",
                    "--sample-size",
                    "10",
                    "--measurement-time",
                    "0.2",
                    "--warm-up-time",
                    "0.1",
                ],
            ),
            PlannedCommand::new(
                "quick_power_thermal",
                "cargo",
                [
                    "bench",
                    "-p",
                    "bootty-app",
                    "--bench",
                    "power_thermal",
                    "power_thermal_idle_prompt_1s_render_model",
                    "--",
                    "--sample-size",
                    "10",
                    "--measurement-time",
                    "0.2",
                    "--warm-up-time",
                    "0.1",
                ],
            ),
        ]);
    }
    commands
}

fn run_logged(planned: &PlannedCommand, output_dir: &Path, summary: &mut File) -> Result<bool> {
    let log_path = output_dir.join(format!("{}.log", planned.name));
    let log = File::create(&log_path)
        .with_context(|| format!("failed to create {}", log_path.display()))?;
    let stderr = log
        .try_clone()
        .with_context(|| format!("failed to clone {}", log_path.display()))?;
    let mut process = planned.command();
    let display = command::display(&process);
    process.stdout(log).stderr(stderr);

    let timer = Timer::start();
    let status = process.status();
    let success = status.as_ref().is_ok_and(std::process::ExitStatus::success);
    let detail = match &status {
        Ok(_) if success => "ok".to_owned(),
        Ok(_) => last_line(&log_path).unwrap_or_else(|| "command failed".to_owned()),
        Err(error) => format!("failed to start {display}: {error}"),
    };
    let record = CommandRecord {
        schema_version: 1,
        event: "benchmark_reproduction_command",
        name: &planned.name,
        status: if success { "pass" } else { "fail" },
        detail,
        duration_s: timer.elapsed().as_secs(),
        exit_code: status.map_or(127, crate::cancellation::exit_code),
        command: display,
        log: log_path.to_string_lossy().into_owned(),
    };
    write_json_line(summary, &record)?;
    Ok(success)
}

fn last_line(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    BufReader::new(file).lines().map_while(Result::ok).last()
}

fn write_json_line(writer: &mut File, value: &impl Serialize) -> Result<()> {
    serde_json::to_writer(&mut *writer, value).context("failed to write benchmark summary")?;
    writer
        .write_all(b"\n")
        .context("failed to write benchmark summary")
}
