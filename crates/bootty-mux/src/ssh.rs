//! Runs a backend's multiplexer client on another host over SSH.
//!
//! The client-server backends already treat the multiplexer as something they talk to rather than
//! something they contain: snapshots and mutations are `tmux`/`zellij` invocations, and a pane is a
//! PTY running an attach client. Both only need their argv prefixed with an SSH invocation to land
//! on the other host, so a remote binding reuses every parser, layout and capability the local one
//! does.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    process::Command,
};

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bootty_config::config::SshRemoteConfig;
use serde::{Deserialize, Serialize};

use super::{
    process::{CommandOutput, CommandRunner, SystemCommandRunner},
    tmux_protocol::shell_quote,
};

/// Seconds to wait for a connection to be established before giving up.
const CONNECT_TIMEOUT: u32 = 5;
/// Seconds between the keepalives that prove an established connection still carries traffic.
const SERVER_ALIVE_INTERVAL: u32 = 5;
/// How many unanswered keepalives end the connection.
const SERVER_ALIVE_COUNT_MAX: u32 = 3;
const REMOTE_EXEC_PROGRAM: &str = "bootty";
const REMOTE_EXEC_SUBCOMMAND: &str = "remote-exec";
const REMOTE_PING_SUBCOMMAND: &str = "remote-ping";
const MAX_REMOTE_COMMAND_PAYLOAD: usize = 1024 * 1024;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
struct RemoteCommand {
    program: String,
    args: Vec<String>,
    terminal: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshRemote {
    config: SshRemoteConfig,
}

impl SshRemote {
    pub fn new(config: SshRemoteConfig) -> Self {
        Self { config }
    }

    pub fn host(&self) -> &str {
        &self.config.host
    }

    /// The SSH destination: `user@host` when the config names a user, and whatever `~/.ssh/config`
    /// resolves otherwise.
    pub fn destination(&self) -> String {
        match &self.config.user {
            Some(user) => format!("{user}@{}", self.config.host),
            None => self.config.host.clone(),
        }
    }

    /// argv for a direct remote-shell command. Bootty uses this only for shell-neutral bootstrap
    /// commands. Backend commands use [`Self::proxy_command`] so their arguments never depend on
    /// the remote login shell.
    pub fn command(&self, program: &str, args: &[String]) -> (String, Vec<String>) {
        self.build_line(remote_command_line(program, args), &["-o", "BatchMode=yes"])
    }

    /// Run one backend command through the remote Bootty executable. The SSH shell sees only
    /// `bootty remote-exec <base64url>`, which is valid in POSIX shells, cmd.exe, and PowerShell.
    pub fn proxy_command(&self, program: &str, args: &[String]) -> Result<(String, Vec<String>)> {
        Ok(self.build_line(
            remote_proxy_command_line_for(REMOTE_EXEC_SUBCOMMAND, program, args, false)?,
            &["-o", "BatchMode=yes"],
        ))
    }

    /// The proxied attach client owns a PTY and may prompt for credentials.
    pub fn proxy_tty_command(
        &self,
        program: &str,
        args: &[String],
    ) -> Result<(String, Vec<String>)> {
        Ok(self.build_line(
            remote_proxy_command_line_for(REMOTE_EXEC_SUBCOMMAND, program, args, true)?,
            &["-t"],
        ))
    }

    fn ping_command(&self) -> (String, Vec<String>) {
        self.build_line(
            format!("{REMOTE_EXEC_PROGRAM} {REMOTE_PING_SUBCOMMAND}"),
            &["-o", "BatchMode=yes"],
        )
    }

    fn build_line(&self, remote_line: String, mode: &[&str]) -> (String, Vec<String>) {
        let mut ssh_args = mode
            .iter()
            .map(|flag| (*flag).to_owned())
            .collect::<Vec<_>>();
        // Configured flags precede bootty's own: SSH keeps the first value it is given for an
        // option, so whatever the host needs wins over the defaults below.
        ssh_args.extend(self.config.args.iter().cloned());
        if let Some(port) = self.config.port {
            ssh_args.push("-p".to_owned());
            ssh_args.push(port.to_string());
        }
        ssh_args.extend(self.keepalive_args());
        ssh_args.extend(self.multiplexing_args());
        ssh_args.push("--".to_owned());
        ssh_args.push(self.destination());
        ssh_args.push(remote_line);
        (self.config.program.clone(), ssh_args)
    }

    /// Turn a lost connection into a failure instead of a wait. A black-holed link answers nothing
    /// and closes nothing: without these, dialing blocks for the operating system's TCP timeout and
    /// an established connection never ends at all, which strands the mutation worker and leaves the
    /// pane showing a session it can no longer reach. Losses are noticed in about
    /// `SERVER_ALIVE_INTERVAL * SERVER_ALIVE_COUNT_MAX` seconds, and the pane reconnects.
    fn keepalive_args(&self) -> Vec<String> {
        vec![
            "-o".to_owned(),
            format!("ConnectTimeout={CONNECT_TIMEOUT}"),
            "-o".to_owned(),
            format!("ServerAliveInterval={SERVER_ALIVE_INTERVAL}"),
            "-o".to_owned(),
            format!("ServerAliveCountMax={SERVER_ALIVE_COUNT_MAX}"),
        ]
    }

    /// Share one connection across invocations, so a mutation issued from a keypress does not pay
    /// for a fresh handshake. Unix only: the control socket is a unix socket, which the SSH client
    /// shipped with Windows does not implement.
    #[cfg(unix)]
    fn multiplexing_args(&self) -> Vec<String> {
        let mut hasher = DefaultHasher::new();
        self.config.program.hash(&mut hasher);
        self.destination().hash(&mut hasher);
        self.config.port.hash(&mut hasher);
        self.config.args.hash(&mut hasher);
        let path = std::env::temp_dir().join(format!("bootty-ssh-{:016x}", hasher.finish()));
        vec![
            "-o".to_owned(),
            "ControlMaster=auto".to_owned(),
            "-o".to_owned(),
            format!("ControlPath={}", path.display()),
            "-o".to_owned(),
            "ControlPersist=60".to_owned(),
        ]
    }

    #[cfg(not(unix))]
    fn multiplexing_args(&self) -> Vec<String> {
        Vec::new()
    }
}

fn remote_command_line(program: &str, args: &[String]) -> String {
    let mut line = shell_quote(program);
    for arg in args {
        line.push(' ');
        line.push_str(&shell_quote(arg));
    }
    line
}

#[cfg(test)]
fn remote_proxy_command_line(program: &str, args: &[String]) -> Result<String> {
    remote_proxy_command_line_for(REMOTE_EXEC_SUBCOMMAND, program, args, false)
}

fn remote_proxy_command_line_for(
    subcommand: &str,
    program: &str,
    args: &[String],
    terminal: bool,
) -> Result<String> {
    let payload = serde_json::to_vec(&RemoteCommand {
        program: program.to_owned(),
        args: args.to_vec(),
        terminal,
    })
    .context("encode remote command")?;
    Ok(format!(
        "{REMOTE_EXEC_PROGRAM} {subcommand} {}",
        URL_SAFE_NO_PAD.encode(payload)
    ))
}

fn decode_remote_command(payload: &str) -> Result<RemoteCommand> {
    if payload.len() > MAX_REMOTE_COMMAND_PAYLOAD {
        bail!("remote command payload is too large")
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .context("decode remote command payload")?;
    let command: RemoteCommand =
        serde_json::from_slice(&bytes).context("parse remote command payload")?;
    if command.program.is_empty() {
        bail!("remote command program cannot be empty")
    }
    Ok(command)
}

pub fn run_remote_command(payload: &str) -> Result<i32> {
    let command = decode_remote_command(payload)?;
    let mut child = Command::new(&command.program);
    child.args(&command.args);
    if command.terminal {
        if let Some(terminfo) = bootty_runtime::terminfo::vendored_terminfo_dir() {
            child.env("TERM", "xterm-bootty").env("TERMINFO", terminfo);
        } else {
            child.env("TERM", "xterm-256color");
        }
    }
    let status = child
        .status()
        .with_context(|| format!("run remote command {}", command.program))?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
pub(crate) fn decode_proxy_command_line(line: &str) -> Result<(String, Vec<String>, bool)> {
    let mut tokens = line.split_whitespace();
    if tokens.next() != Some(REMOTE_EXEC_PROGRAM) || tokens.next() != Some(REMOTE_EXEC_SUBCOMMAND) {
        bail!("not a Bootty remote proxy command")
    }
    let payload = tokens
        .next()
        .context("remote proxy command has no payload")?;
    if tokens.next().is_some() {
        bail!("remote proxy command has extra tokens")
    }
    let command = decode_remote_command(payload)?;
    Ok((command.program, command.args, command.terminal))
}

pub fn remote_bootty_failure(host: &str, detail: &str) -> String {
    let detail = detail.trim();
    let missing = detail.lines().any(|line| {
        line.to_ascii_lowercase().contains("bootty")
            && (line.contains("Unknown command")
                || line.contains("command not found")
                || line.contains("not found"))
    });
    if missing {
        return format!(
            "Bootty is not installed on {host}. Install and open Bootty there, then try again."
        );
    }
    if detail.is_empty() {
        return format!("Could not run Bootty on {host}.");
    }
    format!(
        "Could not run Bootty on {host}: {}",
        detail.lines().next().unwrap_or(detail)
    )
}
pub fn test_ssh_connection(config: &SshRemoteConfig) -> Result<()> {
    test_ssh_connection_with_runner(config, &SystemCommandRunner)
}

fn test_ssh_connection_with_runner<R: CommandRunner>(
    config: &SshRemoteConfig,
    runner: &R,
) -> Result<()> {
    let (program, args) = SshRemote::new(config.clone()).ping_command();
    let output = runner.run(&program, &args)?;
    if output.success {
        return Ok(());
    }

    bail!("{}", remote_bootty_failure(&config.host, &output.stderr))
}

/// Runs every command through [`SshRemote`], for the backends whose own runner has nothing to keep
/// open between invocations.
#[derive(Clone, Debug)]
pub struct SshCommandRunner<R> {
    remote: SshRemote,
    runner: R,
}

impl<R> SshCommandRunner<R> {
    pub fn new(remote: SshRemote, runner: R) -> Self {
        Self { remote, runner }
    }

    fn remote_argv(&self, program: &str, args: &[String]) -> Result<(String, Vec<String>)> {
        self.remote.proxy_command(program, args)
    }
}

impl<R: CommandRunner> CommandRunner for SshCommandRunner<R> {
    fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        let (program, args) = self.remote_argv(program, args)?;
        self.runner.run(&program, &args)
    }

    fn run_disowned(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        let (program, args) = self.remote_argv(program, args)?;
        self.runner.run_disowned(&program, &args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::CommandOutput;
    use std::cell::RefCell;

    fn remote(config: SshRemoteConfig) -> SshRemote {
        SshRemote::new(config)
    }

    fn config(host: &str) -> SshRemoteConfig {
        SshRemoteConfig {
            host: host.to_owned(),
            user: None,
            port: None,
            program: "ssh".to_owned(),
            args: Vec::new(),
        }
    }

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn proxied_command(argv: &[String]) -> RemoteCommand {
        let line = argv.last().expect("remote command line");
        let mut tokens = line.split_whitespace();
        assert_eq!(tokens.next(), Some(REMOTE_EXEC_PROGRAM));
        assert_eq!(tokens.next(), Some(REMOTE_EXEC_SUBCOMMAND));
        let payload = tokens.next().expect("remote command payload");
        assert!(tokens.next().is_none());
        decode_remote_command(payload).expect("decode remote command")
    }

    /// Everything after the destination reaches a remote shell as one string, so the format
    /// strings tmux snapshots depend on have to survive that shell intact.
    #[test]
    fn remote_command_quotes_arguments_for_the_login_shell() {
        let (program, argv) = remote(config("devbox")).command(
            "tmux",
            &args(&["list-sessions", "-F", "s\x1f#{session_id} $HOME"]),
        );

        assert_eq!(program, "ssh");
        assert_eq!(
            argv.last().map(String::as_str),
            Some("'tmux' 'list-sessions' '-F' 's\x1f#{session_id} $HOME'")
        );
        assert!(argv.contains(&"devbox".to_owned()));
    }

    #[test]
    fn connection_test_uses_batch_mode_without_opening_a_live_connection() {
        #[derive(Default)]
        struct Runner {
            call: RefCell<Option<(String, Vec<String>)>>,
        }

        impl CommandRunner for Runner {
            fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
                self.call.replace(Some((program.to_owned(), args.to_vec())));
                Ok(CommandOutput {
                    success: true,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        }

        let runner = Runner::default();
        test_ssh_connection_with_runner(&config("devbox"), &runner).expect("connection test");
        let (_, args) = runner.call.take().expect("SSH invocation");
        assert_eq!(args.first().map(String::as_str), Some("-o"));
        assert_eq!(args.get(1).map(String::as_str), Some("BatchMode=yes"));
        assert_eq!(args.last().map(String::as_str), Some("bootty remote-ping"));
    }

    #[test]
    fn remote_command_line_escapes_embedded_single_quotes() {
        assert_eq!(
            remote_command_line("tmux", &args(&["rename-session", "-t", "it's"])),
            r"'tmux' 'rename-session' '-t' 'it'\''s'"
        );
    }

    #[test]
    fn proxied_command_round_trips_argv_without_remote_shell_quoting() {
        let argv = args(&[
            r"C:\Program Files\backend.exe",
            "rename-session",
            "it's remote",
        ]);
        let line = remote_proxy_command_line(&argv[0], &argv[1..]).expect("encode command");

        assert_eq!(
            decode_remote_command(line.split_whitespace().nth(2).expect("payload"))
                .expect("decode command"),
            RemoteCommand {
                program: argv[0].clone(),
                args: argv[1..].to_vec(),
                terminal: false,
            }
        );
        assert!(
            line.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ' ' | '-' | '_'))
        );
    }

    #[test]
    fn remote_exec_runs_the_decoded_command_without_a_shell() {
        #[cfg(unix)]
        let (program, command_args) = ("sh", args(&["-c", "exit 7"]));
        #[cfg(windows)]
        let (program, command_args) = ("cmd.exe", args(&["/C", "exit 7"]));
        let line = remote_proxy_command_line(program, &command_args).expect("encode command");
        let payload = line.split_whitespace().nth(2).expect("payload");

        assert_eq!(run_remote_command(payload).expect("run command"), 7);
    }

    /// Snapshots poll on a timer with nothing to type a passphrase into; the attach pane is the one
    /// place a prompt can be answered, and the only one that needs a remote terminal.
    #[test]
    fn only_the_attach_client_asks_for_a_tty_and_allows_prompts() {
        let remote = remote(config("devbox"));

        let (_, polled) = remote
            .proxy_command("tmux", &args(&["list-sessions"]))
            .expect("polled command");
        let (_, attached) = remote
            .proxy_tty_command("tmux", &args(&["attach-session"]))
            .expect("attach command");

        assert!(
            polled
                .windows(2)
                .any(|pair| pair == ["-o", "BatchMode=yes"])
        );
        assert!(!polled.contains(&"-t".to_owned()));
        assert!(attached.contains(&"-t".to_owned()));
        assert!(
            !attached
                .windows(2)
                .any(|pair| pair == ["-o", "BatchMode=yes"])
        );
    }

    /// A connection that stops answering has to end rather than hang: the snapshot poll and every
    /// mutation run to completion before the next one starts, so a wait with no timeout strands
    /// them. Configured flags come first, because SSH keeps the first value given for an option and
    /// a host that needs different timings has to be able to say so.
    #[test]
    fn every_connection_is_bounded_and_configured_flags_outrank_the_defaults() {
        let (_, argv) = remote(SshRemoteConfig {
            args: args(&["-o", "ServerAliveInterval=30"]),
            ..config("devbox")
        })
        .command("tmux", &args(&["list-sessions"]));

        let options = argv
            .windows(2)
            .filter(|pair| pair[0] == "-o")
            .map(|pair| pair[1].clone())
            .collect::<Vec<_>>();
        assert!(options.contains(&"ConnectTimeout=5".to_owned()));
        assert!(options.contains(&"ServerAliveCountMax=3".to_owned()));
        assert_eq!(
            options
                .iter()
                .find(|option| option.starts_with("ServerAliveInterval")),
            Some(&"ServerAliveInterval=30".to_owned()),
            "the configured interval has to be the one SSH reads first"
        );
    }

    /// The hosts that need `user`/`port`/`args` are the ones without a usable `~/.ssh/config`, so
    /// each has to reach argv, and `--` must terminate options before the destination.
    #[test]
    fn explicit_credentials_replace_what_ssh_config_would_have_carried() {
        let (_, argv) = remote(SshRemoteConfig {
            user: Some("dev".to_owned()),
            port: Some(2222),
            args: args(&["-i", "C:\\keys\\id_ed25519"]),
            ..config("10.0.0.4")
        })
        .command("tmux", &args(&["list-sessions"]));

        let destination = argv
            .iter()
            .position(|arg| arg == "--")
            .and_then(|index| argv.get(index + 1));
        assert_eq!(destination.map(String::as_str), Some("dev@10.0.0.4"));
        assert!(argv.windows(2).any(|pair| pair == ["-p", "2222"]));
        assert!(
            argv.windows(2)
                .any(|pair| pair == ["-i", "C:\\keys\\id_ed25519"])
        );
    }

    #[derive(Default)]
    struct RecordingRunner {
        calls: RefCell<Vec<(String, Vec<String>)>>,
    }

    impl CommandRunner for RecordingRunner {
        fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
            self.calls
                .borrow_mut()
                .push((program.to_owned(), args.to_vec()));
            Ok(CommandOutput {
                success: true,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn ssh_runner_hands_the_inner_runner_an_ssh_invocation() {
        let runner = SshCommandRunner::new(remote(config("devbox")), RecordingRunner::default());

        runner.run("zellij", &args(&["list-sessions"])).unwrap();

        let calls = runner.runner.calls.borrow();
        assert_eq!(calls[0].0, "ssh");
        assert_eq!(
            proxied_command(&calls[0].1),
            RemoteCommand {
                program: "zellij".to_owned(),
                args: args(&["list-sessions"]),
                terminal: false,
            }
        );
    }
}
