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

use crate::protocol::{TmuxControlNotification, TmuxControlParser};
use bootty_mux::process::{CommandOutput, CommandRunner, SystemCommandRunner};
use bootty_remote::ssh::SshRemote;

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
        let prefix_args = crate::backend::local_server_args(identity);
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

    /// Inject already-encoded terminal bytes into a target pane without asking tmux to interpret
    /// them as keys. The persistent control client keeps this cheap for local and remote panes.
    pub fn send_literal_input(&self, program: &str, target: &str, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let args = ["send-keys", "-H", "-t"]
            .map(str::to_owned)
            .into_iter()
            .chain(std::iter::once(target.to_owned()))
            .chain(bytes.iter().map(|byte| format!("{byte:02x}")))
            .collect::<Vec<_>>();
        let line = literal_input_command_line(&args)?;
        if self.control_command(program, &line, 1).is_some() {
            return Ok(());
        }

        let (program, args) = self.spawned(program, &args);
        let output = SystemCommandRunner.run(&program, &args)?;
        if output.success {
            Ok(())
        } else {
            Err(anyhow!(
                "tmux literal input failed: {}",
                output.stderr.trim()
            ))
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
        if let Some(output) = self.control_query(program, args) {
            Ok(output)
        } else {
            let (program, args) = self.spawned(program, args);
            SystemCommandRunner.run(&program, &args)
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
        std::thread::spawn(move || read_replies(stdout, &replies_tx));

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
        let mut bodies = Vec::with_capacity(blocks);
        for _ in 0..blocks {
            let reply = self.take_reply()?;
            if reply.error {
                return Err(anyhow!(
                    "tmux control client rejected {line:?}: {}",
                    reply.body
                ));
            }
            if !reply.body.is_empty() {
                bodies.push(reply.body);
            }
        }
        Ok(bodies.join("\n"))
    }
}

impl Drop for TmuxControlClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_replies(stdout: ChildStdout, replies: &Sender<Reply>) {
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
        self.control_command(program, &line, blocks)
    }

    fn control_command(&self, program: &str, line: &str, blocks: usize) -> Option<CommandOutput> {
        let mut clients = self.clients.lock().ok()?;
        let slot = clients.entry(self.client_key(program)).or_default();
        if slot.client.is_none() {
            if slot.retry_after.is_some_and(|at| Instant::now() < at) {
                return None;
            }
            if let Ok(client) =
                TmuxControlClient::start_with(program, &self.prefix_args, self.remote.as_ref())
            {
                slot.client = Some(client);
                slot.retry_after = None;
            } else {
                slot.retry_after = Some(Instant::now() + RESTART_BACKOFF);
                return None;
            }
        }

        if let Ok(stdout) = slot.client.as_mut()?.query(line, blocks) {
            Some(CommandOutput {
                success: true,
                stdout,
                stderr: String::new(),
            })
        } else {
            // A client that timed out or errored cannot be trusted to still be in step with its
            // replies, so it goes rather than risk answering the next query with this one's output.
            slot.client = None;
            slot.retry_after = Some(Instant::now() + RESTART_BACKOFF);
            None
        }
    }
    /// A client answers for one tmux server, so the host it runs on is part of its identity.
    fn client_key(&self, program: &str) -> String {
        self.remote.as_ref().map_or_else(
            || format!("{program}\0{}", self.prefix_args.join("\0")),
            |remote| format!("{}@{program}", remote.destination()),
        )
    }
}

fn literal_input_command_line(args: &[String]) -> Result<String> {
    let mut line = String::new();
    for argument in args {
        if argument.contains(['\'', '\n']) {
            return Err(anyhow!(
                "tmux pane selector cannot contain a quote or newline"
            ));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push('\'');
        line.push_str(argument);
        line.push('\'');
    }
    Ok(line)
}

fn spawn_argv(program: &str, args: &[String], remote: Option<&SshRemote>) -> (String, Vec<String>) {
    remote.map_or_else(
        || (program.to_owned(), args.to_vec()),
        |remote| remote.command(program, args),
    )
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
