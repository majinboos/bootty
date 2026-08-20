//! A persistent tmux control-mode client, so the session poll stops spawning a process.
//!
//! Reading the session list is the one tmux call bootty makes on a timer, and every call used to
//! fork a `tmux` client that connected, printed two lists and exited. Control mode keeps one client
//! attached and answers the same queries over its pipe. Anything that changes tmux state still runs
//! as its own process: mutations happen when someone acts, not several times a second.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};

use super::process::{CommandOutput, CommandRunner, SystemCommandRunner};
use super::ssh::SshRemote;
use super::tmux_protocol::{TmuxControlNotification, TmuxControlParser};

/// tmux commands that only read state, and so can be answered by a client shared with every other
/// reader. Everything else keeps its own process, where its exit status and stderr stand alone.
const CONTROL_QUERIES: [&str; 2] = ["list-sessions", "list-panes"];
/// How long a query waits for its reply. A client that misses this is treated as wedged rather than
/// waited on again: the caller forks instead, and the next poll starts a fresh client.
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);
/// How long queries keep forking after a client fails to start or dies. A tmux without control mode,
/// or without a running server, should not be asked for a new client on every poll.
const RESTART_BACKOFF: Duration = Duration::from_secs(10);
/// Handshake answer proving commands reach tmux and replies come back, before any real query trusts
/// the client.
const READY_TOKEN: &str = "bootty-control-ready";

/// Runs read-only tmux queries through a shared control-mode client, and everything else as its own
/// process. Falls back to a process whenever the client is unavailable, so tmux versions without
/// control mode behave exactly as they did before.
#[derive(Clone, Default)]
pub struct TmuxControlRunner {
    clients: Arc<Mutex<HashMap<String, ClientSlot>>>,
    prefix_args: Arc<[String]>,
    /// Set when the tmux server lives on another host. The control client is then a long-lived SSH
    /// process, which is the one place a remote snapshot poll can be as cheap as a local one.
    remote: Option<SshRemote>,
}

impl TmuxControlRunner {
    pub fn for_identity(identity: bootty_identity::ApplicationIdentity) -> Self {
        let prefix_args = crate::tmux::local_server_args(identity);
        Self {
            clients: Arc::default(),
            prefix_args: prefix_args.into(),
            remote: None,
        }
    }

    pub fn for_remote(remote: SshRemote) -> Self {
        Self {
            clients: Arc::default(),
            prefix_args: Arc::default(),
            remote: Some(remote),
        }
    }
}

impl std::fmt::Debug for TmuxControlRunner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TmuxControlRunner")
            .finish_non_exhaustive()
    }
}

impl CommandRunner for TmuxControlRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        match self.control_query(program, args) {
            Some(output) => Ok(output),
            None => {
                let (program, args) = self.spawned(program, args);
                SystemCommandRunner.run(&program, &args)
            }
        }
    }

    fn run_disowned(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        let (program, args) = self.spawned(program, args);
        SystemCommandRunner.run_disowned(&program, &args)
    }
}

struct Reply {
    body: String,
    error: bool,
}

struct TmuxControlClient {
    child: Child,
    stdin: ChildStdin,
    replies: Receiver<Reply>,
}

impl TmuxControlClient {
    fn start(program: &str, prefix_args: &[String], remote: Option<&SshRemote>) -> Result<Self> {
        Self::start_with(program, prefix_args, remote)
    }

    /// `prefix_args` go before `-C`, which is where tmux wants `-L`/`-S`. Only tests pass any.
    fn start_with(
        program: &str,
        prefix_args: &[String],
        remote: Option<&SshRemote>,
    ) -> Result<Self> {
        // No `-t`: the most recently used session is as good as any, since the client is only ever
        // asked about the server as a whole. `no-output` keeps pane data out of the pipe, which
        // bootty reads from its own PTY attachments, and `ignore-size` keeps a client with no
        // terminal from having an opinion about window size.
        let tmux_args = prefix_args
            .iter()
            .cloned()
            .chain(["-C", "attach-session", "-f", "ignore-size,no-output"].map(str::to_owned))
            .collect::<Vec<_>>();
        let (program, args) = spawn_argv(program, &tmux_args, remote);
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("tmux control client has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("tmux control client has no stdout"))?;
        let (replies_tx, replies) = channel();
        std::thread::spawn(move || read_replies(stdout, replies_tx));

        let mut client = Self {
            child,
            stdin,
            replies,
        };
        // Attaching answers with a block of its own; the handshake below is what the first real
        // query's reply must line up behind.
        client.take_reply()?;
        let ready = client.query(&format!("display-message -p {READY_TOKEN}"), 1)?;
        if ready.trim() != READY_TOKEN {
            return Err(anyhow!("tmux control client answered {ready:?}"));
        }
        Ok(client)
    }

    fn take_reply(&self) -> Result<Reply> {
        self.replies
            .recv_timeout(QUERY_TIMEOUT)
            .map_err(|error| anyhow!("tmux control client stopped answering: {error}"))
    }

    /// Submit one command line and join the bodies of the `blocks` replies it produces. tmux answers
    /// commands in order, one block each, and the caller holds the client's lock for the whole
    /// exchange, so replies cannot be read out of turn.
    fn query(&mut self, line: &str, blocks: usize) -> Result<String> {
        writeln!(self.stdin, "{line}")?;
        self.stdin.flush()?;
        let mut body = String::new();
        for _ in 0..blocks {
            let reply = self.take_reply()?;
            if reply.error {
                return Err(anyhow!(
                    "tmux control client rejected {line:?}: {}",
                    reply.body
                ));
            }
            if reply.body.is_empty() {
                continue;
            }
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(&reply.body);
        }
        Ok(body)
    }
}

impl Drop for TmuxControlClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_replies(stdout: ChildStdout, replies: Sender<Reply>) {
    let mut parser = TmuxControlParser::default();
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        // Notifications this build does not model — `%unlinked-window-add`, whatever a newer tmux
        // adds — are not failures. Only the blocks around command replies matter here.
        let Ok(notifications) = parser.put_str(&line) else {
            continue;
        };
        for notification in notifications {
            let reply = match notification {
                TmuxControlNotification::BlockEnd(body) => Reply { body, error: false },
                TmuxControlNotification::BlockError(body) => Reply { body, error: true },
                _ => continue,
            };
            if replies.send(reply).is_err() {
                return;
            }
        }
    }
}

#[derive(Default)]
struct ClientSlot {
    client: Option<TmuxControlClient>,
    retry_after: Option<Instant>,
}

impl TmuxControlRunner {
    /// argv for running `program args...` as its own process: an SSH invocation for a remote
    /// server, and the command itself for a local one.
    fn spawned(&self, program: &str, args: &[String]) -> (String, Vec<String>) {
        let args = self
            .prefix_args
            .iter()
            .cloned()
            .chain(args.iter().cloned())
            .collect::<Vec<_>>();
        spawn_argv(program, &args, self.remote.as_ref())
    }

    /// Answer `args` from this backend's control client, or `None` to let the caller run its own
    /// process. Cloned backends share the client; dropping the last clone drops the registry and
    /// tears every attached client down.
    fn control_query(&self, program: &str, args: &[String]) -> Option<CommandOutput> {
        let line = control_command_line(args)?;
        let blocks = expected_blocks(args);
        let mut clients = self.clients.lock().ok()?;
        let slot = clients.entry(self.client_key(program)).or_default();
        if slot.client.is_none() {
            if slot.retry_after.is_some_and(|at| Instant::now() < at) {
                return None;
            }
            match TmuxControlClient::start(program, &self.prefix_args, self.remote.as_ref()) {
                Ok(client) => {
                    slot.client = Some(client);
                    slot.retry_after = None;
                }
                Err(_) => {
                    slot.retry_after = Some(Instant::now() + RESTART_BACKOFF);
                    return None;
                }
            }
        }

        match slot.client.as_mut()?.query(&line, blocks) {
            Ok(stdout) => Some(CommandOutput {
                success: true,
                stdout,
                stderr: String::new(),
            }),
            // A client that timed out or errored cannot be trusted to still be in step with its
            // replies, so it goes rather than risk answering the next query with this one's output.
            Err(_) => {
                slot.client = None;
                slot.retry_after = Some(Instant::now() + RESTART_BACKOFF);
                None
            }
        }
    }
}

impl TmuxControlRunner {
    /// A client answers for one tmux server, so the host it runs on is part of its identity.
    fn client_key(&self, program: &str) -> String {
        match &self.remote {
            Some(remote) => format!("{}@{program}", remote.destination()),
            None => format!("{program}\0{}", self.prefix_args.join("\0")),
        }
    }
}

fn spawn_argv(program: &str, args: &[String], remote: Option<&SshRemote>) -> (String, Vec<String>) {
    match remote {
        Some(remote) => remote.command(program, args),
        None => (program.to_owned(), args.to_vec()),
    }
}

/// How many reply blocks `args` produces: tmux answers each `;`-separated command with its own.
fn expected_blocks(args: &[String]) -> usize {
    1 + args.iter().filter(|arg| *arg == ";").count()
}

/// The control-mode command line for `args`, or `None` when the control client should not run it.
///
/// Every command has to be a read-only query, and every argument has to survive tmux's parser
/// unchanged. Single quotes keep an argument literal — including the `#{...}` a format string
/// carries — and tmux offers no way to escape a quote inside them, so an argument holding one goes
/// back to being its own process rather than being mangled here.
fn control_command_line(args: &[String]) -> Option<String> {
    if args.is_empty() {
        return None;
    }
    let mut line = String::new();
    for command in args.split(|arg| arg == ";") {
        let (name, arguments) = command.split_first()?;
        if !CONTROL_QUERIES.contains(&name.as_str()) {
            return None;
        }
        if !line.is_empty() {
            line.push_str(" ; ");
        }
        line.push_str(name);
        for argument in arguments {
            if argument.contains('\'') || argument.contains('\n') {
                return None;
            }
            line.push_str(" '");
            line.push_str(argument);
            line.push('\'');
        }
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn control_command_line_quotes_a_chained_snapshot_query() {
        let line = control_command_line(&args(&[
            "list-sessions",
            "-F",
            "s\x1f#{session_id}",
            ";",
            "list-panes",
            "-a",
            "-F",
            "p\x1f#{pane_id}",
        ]))
        .expect("snapshot query");

        assert_eq!(
            line,
            "list-sessions '-F' 's\x1f#{session_id}' ; list-panes '-a' '-F' 'p\x1f#{pane_id}'"
        );
        assert_eq!(
            expected_blocks(&args(&["list-sessions", ";", "list-panes"])),
            2
        );
    }

    /// Mutations skip the control client and fork their own process. For a remote binding that fork
    /// has to be an SSH invocation: run here, a rename or a kill would land on this machine's tmux
    /// server, whose sessions bootty is not showing.
    #[test]
    fn a_remote_runner_forks_its_mutations_at_the_other_host() {
        let remote = SshRemote::new(bootty_mux_model::SshTarget {
            host: "devbox".to_owned(),
            user: None,
            port: None,
            program: "ssh".to_owned(),
            args: Vec::new(),
        });
        let mutation = args(&["kill-session", "-t", "build"]);

        let (program, argv) = TmuxControlRunner::for_remote(remote).spawned("tmux", &mutation);
        assert_eq!(program, "ssh");
        assert_eq!(
            argv.last().map(String::as_str),
            Some("'tmux' 'kill-session' '-t' 'build'")
        );

        let (program, argv) = TmuxControlRunner::default().spawned("tmux", &mutation);
        assert_eq!(program, "tmux");
        assert_eq!(argv, mutation);
    }

    #[test]
    fn control_command_line_refuses_anything_it_would_change_or_mangle() {
        // A mutation answered by the shared client would be run out of band from its own exit
        // status, and a quote inside an argument has no escape in tmux's parser.
        assert_eq!(
            control_command_line(&args(&["rename-session", "-t", "$1", "release"])),
            None
        );
        assert_eq!(
            control_command_line(&args(&["list-sessions", ";", "kill-server"])),
            None
        );
        assert_eq!(
            control_command_line(&args(&["list-sessions", "-F", "it's"])),
            None
        );
        assert_eq!(control_command_line(&[]), None);
    }

    #[cfg(unix)]
    #[test]
    fn dropping_the_backend_ends_its_control_client() {
        use super::super::tmux::TmuxBackend;

        let runner = TmuxControlRunner::default();
        let registry = Arc::downgrade(&runner.clients);
        let mut child = Command::new("sh")
            .args(["-c", "cat >/dev/null"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("start stand-in control client");
        let pid = child.id();
        let stdin = child.stdin.take().expect("client stdin");
        let stdout = child.stdout.take().expect("client stdout");
        let (_replies_tx, replies) = channel();
        runner.clients.lock().expect("client registry").insert(
            "tmux".to_owned(),
            ClientSlot {
                client: Some(TmuxControlClient {
                    child,
                    stdin,
                    replies,
                }),
                retry_after: None,
            },
        );
        let backend = TmuxBackend::with_runner("tmux", runner);

        drop(backend);

        assert!(
            registry.upgrade().is_none(),
            "backend owns the client registry"
        );
        assert!(
            !Command::new("ps")
                .args(["-p", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("inspect stand-in process")
                .success(),
            "dropping the registry must kill and wait for its client"
        );
        drop(stdout);
    }

    /// Both snapshot paths have to describe the same server, or control mode is a data change
    /// wearing a performance change's clothes. This reads the developer's own tmux server, which is
    /// why it is opt-in.
    #[cfg(unix)]
    #[test]
    #[ignore = "reads the running tmux server on the default socket"]
    fn control_mode_and_process_snapshots_describe_the_same_server() {
        use super::super::tmux::TmuxBackend;

        let forked = TmuxBackend::with_runner("tmux", SystemCommandRunner)
            .snapshot()
            .expect("forked snapshot");
        let controlled = TmuxBackend::new().snapshot().expect("control snapshot");

        assert_eq!(forked, controlled);
        assert!(
            !controlled.sessions.is_empty(),
            "start a tmux session before running this"
        );
    }

    /// Skipped where tmux is unavailable; the fallback path covers that case in production too.
    #[cfg(unix)]
    #[test]
    #[ignore = "requires a tmux binary"]
    fn control_client_answers_repeated_queries_from_one_process() {
        let socket = format!("bootty-control-test-{}", std::process::id());
        // Start the server from an empty config on a private socket: whoever runs this has their
        // own `~/.tmux.conf`, and a session hook there runs to completion before `new-session`
        // returns, so a hook that waits on something else would hang this test forever. The
        // server also inherits and holds whatever stdout it is handed, so capturing output would
        // block on an EOF that never comes — send its streams to /dev/null and read the status.
        let tmux = |args: &[&str]| {
            Command::new("tmux")
                .args(["-L", &socket, "-f", "/dev/null"])
                .args(args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
        };
        assert!(
            tmux(&["new-session", "-d", "-s", "one"])
                .expect("start private tmux server")
                .success(),
            "tmux must start a private server"
        );
        struct KillServer(String);
        impl Drop for KillServer {
            fn drop(&mut self) {
                let _ = Command::new("tmux")
                    .args(["-L", &self.0, "kill-server"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
        let _guard = KillServer(socket.clone());

        let mut client =
            TmuxControlClient::start_with("tmux", &args(&["-L", &socket, "-f", "/dev/null"]), None)
                .expect("control client");
        let query = "list-sessions -F '#{session_name}'";

        assert_eq!(client.query(query, 1).expect("first query"), "one");
        assert!(
            tmux(&["new-session", "-d", "-s", "two"])
                .expect("second session")
                .success()
        );
        // The same client, still one process, reports state it was never told about at startup.
        assert_eq!(
            client.query(query, 1).expect("second query"),
            "one\ntwo",
            "a live client should report both sessions"
        );
        // A rejected command must not leave the client answering the next query with its output.
        assert!(client.query("bogus-command", 1).is_err());
        assert_eq!(
            client.query(query, 1).expect("query after error"),
            "one\ntwo"
        );
    }
}
