//! Runs a backend's multiplexer client on another host over SSH.
//!
//! The client-server backends already treat the multiplexer as something they talk to rather than
//! something they contain: snapshots and mutations are `tmux`/`zellij` invocations, and a pane is a
//! PTY running an attach client. Both only need their argv prefixed with an SSH invocation to land
//! on the other host, so a remote binding reuses every parser, layout and capability the local one
//! does.

use std::path::{Path, PathBuf};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    sync::{Arc, Mutex},
};

pub use crate::exec::run_remote_command;
pub use crate::exec::{REMOTE_DAEMON_PROGRAM, REMOTE_DAEMON_PROTOCOL_VERSION};
use crate::exec::{REMOTE_EXEC_PROGRAM, REMOTE_PING_SUBCOMMAND, proxy_command_line};
use anyhow::Result;
use bootty_mux_model::SshTarget;

use bootty_mux::process::{CommandOutput, CommandRunner, SystemCommandRunner};

use crate::shell_quote;

/// Seconds to wait for a connection to be established before giving up.
const CONNECT_TIMEOUT: u32 = 5;
/// Seconds between the keepalives that prove an established connection still carries traffic.
const SERVER_ALIVE_INTERVAL: u32 = 5;
/// How many unanswered keepalives end the connection.
const SERVER_ALIVE_COUNT_MAX: u32 = 3;

#[derive(Clone, Debug)]
pub struct SshRemote {
    config: SshTarget,
    daemon_ready: Arc<Mutex<bool>>,
}

impl PartialEq for SshRemote {
    fn eq(&self, other: &Self) -> bool {
        self.config == other.config
    }
}

impl Eq for SshRemote {}

impl SshRemote {
    pub fn new(config: SshTarget) -> Self {
        Self {
            config,
            daemon_ready: Arc::new(Mutex::new(false)),
        }
    }

    pub fn host(&self) -> &str {
        &self.config.host
    }

    pub fn target(&self) -> &SshTarget {
        &self.config
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
        crate::install::ensure(self, runner)?;
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

    pub(crate) fn raw_command(&self, remote_line: &str) -> (String, Vec<String>) {
        self.build_line(remote_line.to_owned(), &["-o", "BatchMode=yes"])
    }

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
        args.extend(Self::keepalive_args());
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
        ssh_args.extend(Self::keepalive_args());
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
    fn keepalive_args() -> Vec<String> {
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
