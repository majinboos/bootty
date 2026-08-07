//! Runs a backend's multiplexer client on another host over SSH.
//!
//! The client-server backends already treat the multiplexer as something they talk to rather than
//! something they contain: snapshots and mutations are `tmux`/`zellij` invocations, and a pane is a
//! PTY running an attach client. Both only need their argv prefixed with an SSH invocation to land
//! on the other host, so a remote binding reuses every parser, layout and capability the local one
//! does.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use anyhow::Result;
use bootty_config::config::SshRemoteConfig;

use super::{
    process::{CommandOutput, CommandRunner},
    tmux_protocol::shell_quote,
};

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

    /// argv for running `program args...` on the remote host and reading its output. Batch mode is
    /// on: the snapshot poll runs several times a second with no terminal to answer a passphrase
    /// prompt on, and a command that blocks on one would never return.
    pub fn command(&self, program: &str, args: &[String]) -> (String, Vec<String>) {
        self.build(program, args, &["-o", "BatchMode=yes"])
    }

    /// argv for the attach client, which owns a PTY: it asks for a remote terminal, and may prompt
    /// for credentials on the pane the user is looking at.
    pub fn tty_command(&self, program: &str, args: &[String]) -> (String, Vec<String>) {
        self.build(program, args, &["-t"])
    }

    fn build(&self, program: &str, args: &[String], mode: &[&str]) -> (String, Vec<String>) {
        let mut ssh_args = mode
            .iter()
            .map(|flag| (*flag).to_owned())
            .collect::<Vec<_>>();
        ssh_args.extend(self.multiplexing_args());
        if let Some(port) = self.config.port {
            ssh_args.push("-p".to_owned());
            ssh_args.push(port.to_string());
        }
        ssh_args.extend(self.config.args.iter().cloned());
        ssh_args.push(self.destination());
        // SSH joins the remaining argv with spaces and hands the result to the remote login shell,
        // so the command has to arrive already quoted for that shell.
        ssh_args.push("--".to_owned());
        ssh_args.push(remote_command_line(program, args));
        (self.config.program.clone(), ssh_args)
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
}

impl<R: CommandRunner> CommandRunner for SshCommandRunner<R> {
    fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        let (program, args) = self.remote.command(program, args);
        self.runner.run(&program, &args)
    }

    fn run_disowned(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        let (program, args) = self.remote.command(program, args);
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
    fn remote_command_line_escapes_embedded_single_quotes() {
        assert_eq!(
            remote_command_line("tmux", &args(&["rename-session", "-t", "it's"])),
            r"'tmux' 'rename-session' '-t' 'it'\''s'"
        );
    }

    /// Snapshots poll on a timer with nothing to type a passphrase into; the attach pane is the one
    /// place a prompt can be answered, and the only one that needs a remote terminal.
    #[test]
    fn only_the_attach_client_asks_for_a_tty_and_allows_prompts() {
        let remote = remote(config("devbox"));

        let (_, polled) = remote.command("tmux", &args(&["list-sessions"]));
        let (_, attached) = remote.tty_command("tmux", &args(&["attach-session"]));

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

    /// The hosts that need `user`/`port`/`args` are the ones without a usable `~/.ssh/config`, so
    /// each has to reach the argv, and the destination has to stay the last word before `--`.
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
            .and_then(|index| argv.get(index - 1));
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
            calls[0].1.last().map(String::as_str),
            Some("'zellij' 'list-sessions'")
        );
    }
}
