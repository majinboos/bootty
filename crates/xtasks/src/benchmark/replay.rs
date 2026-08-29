use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use clap::Args as ClapArgs;
use sha2::{Digest, Sha256};

use crate::clock::utc_datetime;
use crate::command;

const VERSIONED_APPS: &[&str] = &[
    "nvim", "vim", "helix", "hx", "emacs", "less", "fzf", "git", "tmux", "btop", "htop", "kubectl",
    "docker", "podman", "cargo", "npm", "pytest", "go",
];

#[derive(Clone, Debug, ClapArgs)]
pub struct Args {
    /// Name of the fixture bundle.
    fixture_name: String,

    /// Directory under which the named fixture is created.
    output_root: PathBuf,

    /// Command and arguments to record.
    #[arg(last = true, required = true)]
    command: Vec<OsString>,
}

pub fn run(args: Args) -> Result<()> {
    let Args {
        fixture_name,
        output_root,
        command,
    } = args;
    ensure!(
        matches!(
            Path::new(&fixture_name)
                .components()
                .collect::<Vec<_>>()
                .as_slice(),
            [std::path::Component::Normal(_)]
        ),
        "fixture name must be one path component"
    );
    let script = find_program("script").context("record-replay-fixture: script(1) is required")?;
    let output_dir = output_root.join(&fixture_name);
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    let stream_file = output_dir.join("stream.pty");
    let timing_file = output_dir.join("timing.tsv");
    let metadata_file = output_dir.join("metadata.env");
    let start_ns = epoch_nanoseconds()?;
    write_metadata(&metadata_file, &fixture_name, &command)?;

    let util_linux = supports_timing_file(&script);
    if !util_linux {
        fs::write(&timing_file, format!("start_ns\t{start_ns}\n"))
            .with_context(|| format!("failed to create {}", timing_file.display()))?;
    }
    let mut recorder = Command::new(&script);
    if util_linux {
        recorder
            .args([OsStr::new("-q"), OsStr::new("-e"), OsStr::new("-T")])
            .arg(&timing_file)
            .arg("-c")
            .arg(shell_command(&command)?)
            .arg(&stream_file);
    } else {
        recorder.arg("-q").arg(&stream_file).args(&command);
    }
    command::run(&mut recorder).context("failed to record replay fixture")?;

    let end_ns = epoch_nanoseconds()?;
    let mut timing = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&timing_file)
        .with_context(|| format!("failed to open {}", timing_file.display()))?;
    writeln!(timing, "end_ns\t{end_ns}")?;
    writeln!(timing, "duration_ns\t{}", end_ns.saturating_sub(start_ns))?;

    write_checksums(&output_dir)?;
    println!("Recorded fixture bundle: {}", output_dir.display());
    Ok(())
}

fn write_metadata(path: &Path, fixture_name: &str, command_args: &[OsString]) -> Result<()> {
    let mut metadata =
        File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    let cols = terminal_dimension("COLUMNS", "cols", "80");
    let rows = terminal_dimension("LINES", "lines", "24");
    let command = shell_command(command_args)?;

    write_env(&mut metadata, "fixture", OsStr::new(fixture_name))?;
    write_env(
        &mut metadata,
        "recorded_at_utc",
        OsStr::new(&utc_datetime()?),
    )?;
    write_env(&mut metadata, "cols", OsStr::new(&cols))?;
    write_env(&mut metadata, "rows", OsStr::new(&rows))?;
    write_env(
        &mut metadata,
        "term",
        env::var_os("TERM")
            .as_deref()
            .unwrap_or(OsStr::new("unknown")),
    )?;
    write_env(&mut metadata, "command", OsStr::new(&command))?;
    writeln!(metadata)?;
    write_env(
        &mut metadata,
        "uname",
        OsStr::new(&command_stdout("uname", &["-a"])),
    )?;
    write_env(
        &mut metadata,
        "shell",
        env::var_os("SHELL")
            .as_deref()
            .unwrap_or(OsStr::new("unknown")),
    )?;
    for app in VERSIONED_APPS {
        if let Some(program) = find_program(app) {
            let version = first_version_line(&program);
            write_env(
                &mut metadata,
                &format!("version_{app}"),
                OsStr::new(&version),
            )?;
        }
    }
    Ok(())
}

fn terminal_dimension(variable: &str, tput_name: &str, fallback: &str) -> String {
    env::var(variable)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            let mut tput = Command::new("tput");
            tput.arg(tput_name).stdin(Stdio::null());
            command::stdout(&mut tput)
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| fallback.to_owned())
        })
}

fn supports_timing_file(script: &Path) -> bool {
    Command::new(script)
        .arg("--help")
        .output()
        .is_ok_and(|output| {
            String::from_utf8_lossy(&output.stdout).contains("-T")
                || String::from_utf8_lossy(&output.stderr).contains("-T")
        })
}

fn first_version_line(program: &Path) -> String {
    let output = Command::new(program).arg("--version").output();
    let Ok(output) = output else {
        return "unknown".to_owned();
    };
    if !output.status.success() {
        return "unknown".to_owned();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    stdout
        .lines()
        .chain(stderr.lines())
        .next()
        .unwrap_or("unknown")
        .trim_end_matches('\r')
        .to_owned()
}

fn command_stdout(program: &str, args: &[&str]) -> String {
    let mut process = Command::new(program);
    process.args(args).stdin(Stdio::null());
    command::stdout(&mut process).map_or_else(
        |_| "unknown".to_owned(),
        |value| value.trim_end().to_owned(),
    )
}

fn write_env(writer: &mut File, name: &str, value: &OsStr) -> Result<()> {
    writeln!(writer, "{name}={}", shell_quote(&value.to_string_lossy()))
        .context("failed to write replay metadata")
}

fn shell_command(command: &[OsString]) -> Result<String> {
    command
        .iter()
        .map(|part| {
            part.to_str()
                .map(shell_quote)
                .context("replay command arguments must be valid UTF-8")
        })
        .collect::<Result<Vec<_>>>()
        .map(|parts| parts.join(" "))
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&byte))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn find_program(program: impl AsRef<OsStr>) -> Option<PathBuf> {
    let program = Path::new(program.as_ref());
    if program.components().count() > 1 {
        return program.is_file().then(|| program.to_owned());
    }
    env::split_paths(&env::var_os("PATH")?)
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
}

fn epoch_nanoseconds() -> Result<u128> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")
        .map(|duration| duration.as_nanos())
}

fn write_checksums(output_dir: &Path) -> Result<()> {
    let checksum_path = output_dir.join("SHA256SUMS");
    let mut checksums = File::create(&checksum_path)
        .with_context(|| format!("failed to create {}", checksum_path.display()))?;
    for name in ["stream.pty", "timing.tsv", "metadata.env"] {
        let mut file = File::open(output_dir.join(name))
            .with_context(|| format!("failed to open {name} for checksumming"))?;
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        let digest = hasher.finalize();
        for byte in digest {
            write!(checksums, "{byte:02x}")?;
        }
        writeln!(checksums, "  {name}")?;
    }
    Ok(())
}
