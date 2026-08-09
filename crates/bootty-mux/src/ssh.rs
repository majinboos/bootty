//! Runs a backend's multiplexer client on another host over SSH.
//!
//! The client-server backends already treat the multiplexer as something they talk to rather than
//! something they contain: snapshots and mutations are `tmux`/`zellij` invocations, and a pane is a
//! PTY running an attach client. Both only need their argv prefixed with an SSH invocation to land
//! on the other host, so a remote binding reuses every parser, layout and capability the local one
//! does.

#[cfg(feature = "remote-install")]
use std::path::{Path, PathBuf};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    sync::{Arc, Mutex},
};

pub use crate::remote_exec::run_remote_command;
pub use crate::remote_exec::{REMOTE_DAEMON_PROGRAM, REMOTE_DAEMON_PROTOCOL_VERSION};
use crate::remote_exec::{REMOTE_EXEC_PROGRAM, REMOTE_PING_SUBCOMMAND, proxy_command_line};
#[cfg(test)]
use crate::remote_exec::{REMOTE_EXEC_SUBCOMMAND, RemoteCommand, decode_remote_command};
use anyhow::Result;
use bootty_config::config::SshRemoteConfig;

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

#[derive(Clone, Debug)]
pub struct SshRemote {
    config: SshRemoteConfig,
    daemon_ready: Arc<Mutex<bool>>,
}

impl PartialEq for SshRemote {
    fn eq(&self, other: &Self) -> bool {
        self.config == other.config
    }
}

impl Eq for SshRemote {}

impl SshRemote {
    pub fn new(config: SshRemoteConfig) -> Self {
        Self {
            config,
            daemon_ready: Arc::new(Mutex::new(false)),
        }
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

    pub fn ensure_daemon(&self) -> Result<()> {
        self.ensure_daemon_with(&SystemCommandRunner)
    }

    pub fn ensure_daemon_with<R: CommandRunner>(&self, runner: &R) -> Result<()> {
        let mut ready = self
            .daemon_ready
            .lock()
            .map_err(|_| anyhow::anyhow!("remote daemon installer lock is poisoned"))?;
        if *ready {
            return Ok(());
        }
        #[cfg(feature = "remote-install")]
        crate::remote_install::ensure(self, runner)?;
        #[cfg(not(feature = "remote-install"))]
        let _ = runner;
        *ready = true;
        Ok(())
    }

    /// argv for a direct remote-shell command. Bootty uses this only for target-specific bootstrap
    /// commands. Backend commands use [`Self::proxy_command`] so their arguments never depend on
    /// the remote login shell.
    pub fn command(&self, program: &str, args: &[String]) -> (String, Vec<String>) {
        self.build_line(remote_command_line(program, args), &["-o", "BatchMode=yes"])
    }

    /// Run one backend command through the remote Bootty daemon. The SSH shell sees one
    /// versioned daemon path plus a base64url payload, which is valid in POSIX shells,
    /// cmd.exe, and PowerShell.
    pub fn proxy_command(&self, program: &str, args: &[String]) -> Result<(String, Vec<String>)> {
        Ok(self.build_line(
            proxy_command_line(program, args, false)?,
            &["-o", "BatchMode=yes"],
        ))
    }

    /// The proxied attach client owns a PTY and may prompt for credentials.
    pub fn proxy_tty_command(
        &self,
        program: &str,
        args: &[String],
    ) -> Result<(String, Vec<String>)> {
        Ok(self.build_line(proxy_command_line(program, args, true)?, &["-t"]))
    }

    pub(crate) fn ping_command(&self) -> (String, Vec<String>) {
        self.build_line(
            format!("{REMOTE_EXEC_PROGRAM} {REMOTE_PING_SUBCOMMAND}"),
            &["-o", "BatchMode=yes"],
        )
    }

    #[cfg(feature = "remote-install")]
    pub(crate) fn raw_command(&self, remote_line: &str) -> (String, Vec<String>) {
        self.build_line(remote_line.to_owned(), &["-o", "BatchMode=yes"])
    }

    #[cfg(feature = "remote-install")]
    pub(crate) fn scp_command(
        &self,
        local_path: &Path,
        remote_path: &str,
    ) -> (String, Vec<String>) {
        let ssh = Path::new(&self.config.program);
        let scp_name = if cfg!(windows) { "scp.exe" } else { "scp" };
        let program = match ssh.file_name().and_then(|name| name.to_str()) {
            Some("ssh" | "ssh.exe") => ssh.with_file_name(scp_name),
            _ => PathBuf::from(scp_name),
        };
        let mut args = self.config.args.clone();
        args.extend(["-o".to_owned(), "BatchMode=yes".to_owned()]);
        if let Some(port) = self.config.port {
            args.push("-P".to_owned());
            args.push(port.to_string());
        }
        args.extend(self.keepalive_args());
        args.push("--".to_owned());
        args.push(local_path.to_string_lossy().into_owned());
        args.push(format!("{}:{remote_path}", self.destination()));
        (program.to_string_lossy().into_owned(), args)
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
    proxy_command_line(program, args, false)
}

#[cfg(test)]
pub(crate) fn decode_proxy_command_line(line: &str) -> Result<(String, Vec<String>, bool)> {
    let command = crate::remote_exec::decode_proxy_command_line(line)?;
    Ok((command.program, command.args, command.terminal))
}

pub fn remote_daemon_failure(host: &str, detail: &str) -> String {
    let detail = detail.trim();
    if detail.is_empty() {
        return format!("Could not run the Bootty daemon on {host}.");
    }
    format!(
        "Could not run the Bootty daemon on {host}: {}",
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
    SshRemote::new(config.clone()).ensure_daemon_with(runner)
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
        self.remote.ensure_daemon_with(&self.runner)?;
        let (program, args) = self.remote_argv(program, args)?;
        self.runner.run(&program, &args)
    }

    fn run_disowned(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        self.remote.ensure_daemon_with(&self.runner)?;
        let (program, args) = self.remote_argv(program, args)?;
        self.runner.run_disowned(&program, &args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::CommandOutput;
    use std::{
        cell::RefCell,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

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
                    stdout: format!(
                        "{REMOTE_DAEMON_PROTOCOL_VERSION}:{}",
                        env!("CARGO_PKG_VERSION")
                    ),
                    stderr: String::new(),
                })
            }
        }

        let runner = Runner::default();
        test_ssh_connection_with_runner(&config("devbox"), &runner).expect("connection test");
        let (_, args) = runner.call.take().expect("SSH invocation");
        assert_eq!(args.first().map(String::as_str), Some("-o"));
        assert_eq!(args.get(1).map(String::as_str), Some("BatchMode=yes"));
        assert_eq!(
            args.last(),
            Some(&format!("{REMOTE_EXEC_PROGRAM} {REMOTE_PING_SUBCOMMAND}"))
        );
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
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ' ' | '-' | '_' | '.' | '/'))
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

    #[test]
    fn cloned_remote_serializes_daemon_installation() {
        #[derive(Clone)]
        struct PingRunner(Arc<AtomicUsize>);

        impl CommandRunner for PingRunner {
            fn run(&self, _program: &str, args: &[String]) -> Result<CommandOutput> {
                assert!(args.last().expect("ping").ends_with("remote-ping"));
                self.0.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(10));
                Ok(CommandOutput {
                    success: true,
                    stdout: format!(
                        "{}:{}",
                        REMOTE_DAEMON_PROTOCOL_VERSION,
                        env!("CARGO_PKG_VERSION")
                    ),
                    stderr: String::new(),
                })
            }
        }

        let remote = remote(config("devbox"));
        let calls = Arc::new(AtomicUsize::new(0));
        let handles = (0..4)
            .map(|_| {
                let remote = remote.clone();
                let runner = PingRunner(calls.clone());
                std::thread::spawn(move || remote.ensure_daemon_with(&runner))
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().expect("installer thread").expect("install");
        }

        assert_eq!(calls.load(Ordering::SeqCst), 1);
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
                stdout: if args.last().is_some_and(|arg| arg.ends_with("remote-ping")) {
                    format!(
                        "{}:{}",
                        REMOTE_DAEMON_PROTOCOL_VERSION,
                        env!("CARGO_PKG_VERSION")
                    )
                } else {
                    String::new()
                },
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn ssh_runner_hands_the_inner_runner_an_ssh_invocation() {
        let runner = SshCommandRunner::new(remote(config("devbox")), RecordingRunner::default());

        runner.run("zellij", &args(&["list-sessions"])).unwrap();

        let calls = runner.runner.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "ssh");
        assert!(calls[0].1.last().expect("ping").ends_with("remote-ping"));
        assert_eq!(
            proxied_command(&calls[1].1),
            RemoteCommand {
                program: "zellij".to_owned(),
                args: args(&["list-sessions"]),
                terminal: false,
            }
        );
    }
}
