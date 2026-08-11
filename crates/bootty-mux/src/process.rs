use anyhow::{Context, Result, bail};
use std::{
    io::{self, Read},
    process::{Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

#[cfg(target_os = "macos")]
use std::{env, ffi::OsStr, path::Path};

#[cfg(target_os = "macos")]
use std::{sync::atomic::AtomicU64, time::Instant};

#[cfg(all(unix, not(target_os = "macos")))]
use std::os::unix::process::CommandExt;

#[cfg(feature = "app")]
use crate::{
    backend::{MuxEvent, MuxEventCapability, MuxEventTopic},
    controller::MuxScope,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

pub trait CommandRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput>;

    fn run_disowned(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        self.run(program, args)
    }

    #[cfg(feature = "app")]
    fn mux_event_capabilities(&self) -> Vec<MuxEventCapability> {
        MuxEventTopic::ALL
            .into_iter()
            .map(|topic| {
                MuxEventCapability::unsupported(
                    topic,
                    "command runner has no persistent backend event transport",
                )
            })
            .collect()
    }

    /// Starts a long-lived event transport before any cursor is drained. Runners without one keep
    /// the default no-op and advertise every topic as unsupported.
    #[cfg(feature = "app")]
    fn start_mux_event_stream(&self, _program: &str) {}
    #[cfg(feature = "app")]
    fn drain_mux_events(&self, _scope: MuxScope, _maximum: usize) -> Vec<MuxEvent> {
        Vec::new()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        command_output(program, Command::new(program).args(args).output())
    }

    fn run_disowned(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        disowned_command_output(program, args)
    }
}
#[derive(Clone, Debug, Default)]
pub struct CommandCancellation(Arc<AtomicBool>);

impl CommandCancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
}

#[derive(Clone, Debug)]
pub struct CancellableCommandRunner {
    cancellation: CommandCancellation,
}

impl CancellableCommandRunner {
    pub fn new(cancellation: CommandCancellation) -> Self {
        Self { cancellation }
    }
}

impl CommandRunner for CancellableCommandRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        cancellable_command_output(program, args, &self.cancellation)
    }
}

fn cancellable_command_output(
    program: &str,
    args: &[String],
    cancellation: &CommandCancellation,
) -> Result<CommandOutput> {
    if cancellation.0.load(Ordering::Acquire) {
        bail!("command canceled")
    }
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("run {program}"))?;
    let stdout = read_pipe(child.stdout.take().context("capture stdout")?);
    let stderr = read_pipe(child.stderr.take().context("capture stderr")?);
    let status = loop {
        if cancellation.0.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = join_pipe(stdout);
            let _ = join_pipe(stderr);
            bail!("command canceled")
        }
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("wait for {program}"))?
        {
            break status;
        }
        thread::sleep(Duration::from_millis(10));
    };
    command_output(
        program,
        Ok(Output {
            status,
            stdout: join_pipe(stdout)?,
            stderr: join_pipe(stderr)?,
        }),
    )
}

fn read_pipe(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

fn join_pipe(reader: thread::JoinHandle<io::Result<Vec<u8>>>) -> Result<Vec<u8>> {
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("command output reader stopped"))?
        .context("read command output")
}

#[cfg(target_os = "macos")]
static DISOWNED_COMMAND_COUNTER: AtomicU64 = AtomicU64::new(0);

#[cfg(target_os = "macos")]
const DISOWNED_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(target_os = "macos")]
const LAUNCHD_START_GRACE: Duration = Duration::from_millis(50);

#[cfg(target_os = "macos")]
const LAUNCHD_SUBMIT_SCRIPT: &str = r#"program=$1
shift
exec "$program" "$@"
"#;

#[cfg(target_os = "macos")]
fn disowned_command_output(program: &str, args: &[String]) -> Result<CommandOutput> {
    let resolved_program = resolve_program(program)?;
    let launchctl = resolve_program("launchctl")?;
    let shell = resolve_program("sh")?;
    let id = DISOWNED_COMMAND_COUNTER.fetch_add(1, Ordering::Relaxed);
    let label = format!("dev.bootty.disowned.{}.{}", std::process::id(), id);
    let script = launchd_submit_script();

    let output = command_output(
        "launchctl",
        Command::new(&launchctl)
            .args(["submit", "-l", &label, "--", &shell, "-c"])
            .arg(script)
            .args(["bootty-disowned", &resolved_program])
            .args(args)
            .output(),
    )?;
    if !output.success {
        return Ok(output);
    }

    let status = wait_for_launchd_exit(&launchctl, &label, DISOWNED_COMMAND_TIMEOUT)
        .with_context(|| format!("wait for disowned {program}"));
    let _ = Command::new(&launchctl).args(["remove", &label]).output();
    status.map(command_status_output)
}

#[cfg(target_os = "macos")]
fn launchd_submit_script() -> String {
    let mut script = macos_shell_environment_prelude();
    script.push_str(LAUNCHD_SUBMIT_SCRIPT);
    script
}

#[cfg(target_os = "macos")]
pub fn macos_shell_environment_prelude() -> String {
    macos_shell_environment_prelude_from(env::vars_os())
}

#[cfg(target_os = "macos")]
pub fn macos_shell_environment_prelude_from<I, K, V>(vars: I) -> String
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    let mut script = String::new();
    for (key, value) in vars {
        let key = key.as_ref().to_string_lossy();
        if !is_shell_identifier(&key) {
            continue;
        }
        script.push_str(&key);
        script.push('=');
        script.push_str(&shell_single_quote(&value.as_ref().to_string_lossy()));
        script.push_str("; export ");
        script.push_str(&key);
        script.push('\n');
    }
    script
}

#[cfg(target_os = "macos")]
fn is_shell_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

#[cfg(target_os = "macos")]
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(target_os = "macos")]
pub fn wait_for_launchd_exit(launchctl: &str, label: &str, timeout: Duration) -> Result<i32> {
    let start = Instant::now();
    let deadline = start + timeout;
    let mut observed_pid = false;
    while Instant::now() < deadline {
        let output = Command::new(launchctl).args(["list", label]).output()?;
        let text = String::from_utf8_lossy(&output.stdout);
        if text.contains("\"PID\"") {
            observed_pid = true;
        } else if observed_pid || start.elapsed() >= LAUNCHD_START_GRACE {
            return parse_launchd_exit_status(&text)
                .with_context(|| format!("parse launchd status for {label}"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    bail!("disowned command did not exit before timeout")
}

#[cfg(target_os = "macos")]
fn parse_launchd_exit_status(text: &str) -> Result<i32> {
    text.lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("\"LastExitStatus\" = ")
                .and_then(|value| value.trim_end_matches(';').parse().ok())
        })
        .context("missing LastExitStatus")
}

#[cfg(target_os = "macos")]
fn command_status_output(status: i32) -> CommandOutput {
    CommandOutput {
        success: status == 0,
        stdout: String::new(),
        stderr: if status == 0 {
            String::new()
        } else {
            format!("process exited with status {status}")
        },
    }
}

#[cfg(target_os = "macos")]
pub fn resolve_program(program: &str) -> Result<String> {
    resolve_program_with_path(program, env::var_os("PATH").as_deref())
}

#[cfg(target_os = "macos")]
fn resolve_program_with_path(program: &str, path: Option<&OsStr>) -> Result<String> {
    if Path::new(program).is_absolute() || program.contains(std::path::MAIN_SEPARATOR) {
        return Ok(program.to_owned());
    }
    if let Some(found) = path
        .into_iter()
        .flat_map(env::split_paths)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
    {
        return Ok(found.to_string_lossy().into_owned());
    }
    bail!("program {program:?} not found in PATH")
}

#[cfg(not(target_os = "macos"))]
fn disowned_command_output(program: &str, args: &[String]) -> Result<CommandOutput> {
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::null());

    #[cfg(unix)]
    command.process_group(0);

    command_output(program, command.output())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn disowned_program_resolution_searches_path_for_bare_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = dir.path().join("tmux");
        std::fs::write(&program, "").expect("write executable placeholder");

        let resolved = resolve_program_with_path("tmux", Some(dir.path().as_os_str()))
            .expect("program should resolve from supplied PATH");

        assert_eq!(Path::new(&resolved).file_name(), Some(OsStr::new("tmux")));
        assert!(Path::new(&resolved).is_file());
    }

    #[test]
    fn disowned_program_resolution_keeps_relative_paths() {
        assert_eq!(resolve_program_with_path("./tmux", None).unwrap(), "./tmux");
    }

    #[test]
    fn disowned_command_preserves_bootty_environment_for_child() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let program = dir.path().join("bootty-env-probe");
        let captured_path = dir.path().join("captured-path");
        let captured_custom = dir.path().join("captured-custom");
        std::fs::write(
            &program,
            "#!/bin/sh\nprintf '%s' \"$PATH\" > \"$1\"\nprintf '%s' \"$BOOTTY_ENV_PROBE\" > \"$2\"",
        )
        .expect("write env probe");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("make env probe executable");
        let old_path = env::var_os("PATH").unwrap_or_default();
        let old_custom = env::var_os("BOOTTY_ENV_PROBE");
        let next_path = env::join_paths(
            std::iter::once(dir.path().to_path_buf()).chain(env::split_paths(&old_path)),
        )
        .expect("join PATH");
        struct EnvGuard {
            path: std::ffi::OsString,
            custom: Option<std::ffi::OsString>,
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                unsafe {
                    env::set_var("PATH", &self.path);
                    if let Some(value) = &self.custom {
                        env::set_var("BOOTTY_ENV_PROBE", value);
                    } else {
                        env::remove_var("BOOTTY_ENV_PROBE");
                    }
                }
            }
        }
        let _guard = EnvGuard {
            path: old_path,
            custom: old_custom,
        };
        unsafe {
            env::set_var("PATH", next_path);
            env::set_var("BOOTTY_ENV_PROBE", "login-env-value");
        }

        let output = disowned_command_output(
            "bootty-env-probe",
            &[
                captured_path.to_string_lossy().into_owned(),
                captured_custom.to_string_lossy().into_owned(),
            ],
        )
        .expect("run disowned env probe");

        assert!(output.success, "probe failed: {}", output.stderr);
        let child_path = std::fs::read_to_string(captured_path).expect("captured PATH");
        assert!(
            env::split_paths(OsStr::new(&child_path)).any(|entry| entry == dir.path()),
            "child PATH did not include prepended temp dir: {child_path}"
        );
        assert_eq!(
            std::fs::read_to_string(captured_custom).expect("captured custom env"),
            "login-env-value"
        );
    }
}

fn command_output(program: &str, output: std::io::Result<Output>) -> Result<CommandOutput> {
    let output = output.with_context(|| format!("run {program}"))?;
    Ok(CommandOutput {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

pub fn require_success(_program: &str, _args: &[String], output: CommandOutput) -> Result<String> {
    if output.success {
        return Ok(output.stdout);
    }

    let detail = output.stderr.trim();
    if detail.is_empty() {
        bail!("command failed")
    }
    bail!("{detail}")
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;

    #[test]
    fn canceled_runner_does_not_start_a_command() {
        let cancellation = CommandCancellation::default();
        cancellation.cancel();
        let runner = CancellableCommandRunner::new(cancellation);

        assert!(
            runner
                .run("bootty-command-that-must-not-exist", &[])
                .unwrap_err()
                .to_string()
                .contains("canceled")
        );
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_stops_a_running_command() {
        let cancellation = CommandCancellation::default();
        let runner = CancellableCommandRunner::new(cancellation.clone());
        let worker =
            thread::spawn(move || runner.run("sh", &["-c".to_owned(), "sleep 10".to_owned()]));
        thread::sleep(Duration::from_millis(50));

        cancellation.cancel();
        let error = worker
            .join()
            .expect("command worker")
            .expect_err("canceled command");

        assert!(error.to_string().contains("canceled"));
    }
}
