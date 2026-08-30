use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use bootty_mux::process::{CommandRunner, SystemCommandRunner, require_success};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::model::HerdrSessionSnapshot;

const HERDR_PROGRAM: &str = "herdr";
#[cfg(unix)]
const API_TIMEOUT: Duration = Duration::from_secs(2);
const SERVER_READY_TIMEOUT: Duration = Duration::from_secs(10);
const SERVER_READY_POLL: Duration = Duration::from_millis(50);
static REQUEST_ID: AtomicU64 = AtomicU64::new(1);
static SERVER_BOOTSTRAP_LOCK: Mutex<()> = Mutex::new(());

pub trait HerdrApi {
    fn snapshot(&self) -> Result<HerdrSessionSnapshot>;
    fn request(&self, method: &str, params: Value) -> Result<Value>;
}

#[derive(Clone, Debug)]
pub struct CliHerdrApi<R = SystemCommandRunner> {
    session: String,
    runner: R,
    bootstrap: bool,
}

impl CliHerdrApi<SystemCommandRunner> {
    pub fn new(session: impl Into<String>) -> Self {
        Self {
            session: session.into(),
            runner: SystemCommandRunner,
            bootstrap: true,
        }
    }
}

impl<R> CliHerdrApi<R> {
    pub fn with_runner(session: impl Into<String>, runner: R) -> Self {
        Self {
            session: session.into(),
            runner,
            bootstrap: false,
        }
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    pub fn runner(&self) -> &R {
        &self.runner
    }
}

impl<R: CommandRunner> HerdrApi for CliHerdrApi<R> {
    fn snapshot(&self) -> Result<HerdrSessionSnapshot> {
        self.ensure_server()?;
        let args = self.args(["api", "snapshot"]);
        let output = self.runner.run(HERDR_PROGRAM, &args)?;
        let stdout = require_success(HERDR_PROGRAM, &args, output)
            .context("read Herdr session.snapshot (requires Herdr 0.8.2 or newer)")?;
        parse_cli_snapshot(&stdout)
    }

    fn request(&self, method: &str, params: Value) -> Result<Value> {
        let socket = self.socket_path()?;
        request_socket(&socket, method, &params)
    }
}

impl<R: CommandRunner> CliHerdrApi<R> {
    fn ensure_server(&self) -> Result<()> {
        if !self.bootstrap {
            return Ok(());
        }
        let _guard = SERVER_BOOTSTRAP_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("Herdr bootstrap lock poisoned"))?;
        if self.server_running()? {
            return Ok(());
        }

        let args = self.args(["server"]);
        let mut command = Command::new(HERDR_PROGRAM);
        command
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn().context("start headless Herdr server")?;
        let deadline = Instant::now() + SERVER_READY_TIMEOUT;
        while Instant::now() < deadline {
            if self.server_running()? {
                // Herdr's public `server` command is the long-lived headless server. A detached
                // waiter reaps it whenever the durable named session is eventually stopped.
                thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(());
            }
            if let Some(status) = child.try_wait().context("wait for headless Herdr server")? {
                bail!("headless Herdr server exited during startup with {status}")
            }
            thread::sleep(SERVER_READY_POLL);
        }
        let _ = child.kill();
        let _ = child.wait();
        bail!(
            "headless Herdr server {:?} did not become ready within {} seconds",
            self.session,
            SERVER_READY_TIMEOUT.as_secs()
        )
    }

    fn server_running(&self) -> Result<bool> {
        let args = self.args(["status", "--json"]);
        let output = self.runner.run(HERDR_PROGRAM, &args)?;
        let stdout =
            require_success(HERDR_PROGRAM, &args, output).context("read Herdr server status")?;
        let value: Value = serde_json::from_str(&stdout).context("decode `herdr status --json`")?;
        value
            .pointer("/server/running")
            .and_then(Value::as_bool)
            .context("`herdr status --json` omitted server.running")
    }

    fn args<'a>(&self, tail: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        let mut args = vec!["--session".to_owned(), self.session.clone()];
        args.extend(tail.into_iter().map(str::to_owned));
        args
    }

    pub fn socket_path(&self) -> Result<String> {
        self.ensure_server()?;
        let args = self.args(["status", "server"]);
        let output = self.runner.run(HERDR_PROGRAM, &args)?;
        let stdout =
            require_success(HERDR_PROGRAM, &args, output).context("resolve Herdr API socket")?;
        stdout
            .lines()
            .find_map(|line| line.strip_prefix("socket: "))
            .map(str::to_owned)
            .context("`herdr status server` omitted socket path")
    }
}

pub(crate) fn parse_snapshot_value(value: &Value) -> Result<HerdrSessionSnapshot> {
    let snapshot = value
        .pointer("/result/snapshot")
        .or_else(|| value.get("snapshot"))
        .unwrap_or(value);
    serde_json::from_value(snapshot.clone()).context("decode Herdr session.snapshot")
}

fn parse_cli_snapshot(stdout: &str) -> Result<HerdrSessionSnapshot> {
    let value: Value = serde_json::from_str(stdout).context("decode `herdr api snapshot`")?;
    parse_snapshot_value(&value)
}

#[derive(Deserialize)]
struct ApiEnvelope {
    id: String,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<ApiError>,
}

#[derive(Deserialize)]
struct ApiError {
    code: String,
    message: String,
}

#[cfg(unix)]
pub(crate) fn request_socket(socket: &str, method: &str, params: &Value) -> Result<Value> {
    use std::os::unix::net::UnixStream;

    let id = format!("bootty-{}", REQUEST_ID.fetch_add(1, Ordering::Relaxed));
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connect to Herdr API socket {socket}"))?;
    stream
        .set_read_timeout(Some(API_TIMEOUT))
        .context("set Herdr API read timeout")?;
    stream
        .set_write_timeout(Some(API_TIMEOUT))
        .context("set Herdr API write timeout")?;
    serde_json::to_writer(
        &mut stream,
        &json!({ "id": id, "method": method, "params": params }),
    )
    .context("encode Herdr API request")?;
    stream.write_all(b"\n").context("write Herdr API request")?;
    stream.flush().context("flush Herdr API request")?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .context("read Herdr API response")?;
    let response: ApiEnvelope = serde_json::from_str(&line).context("decode Herdr API response")?;
    if response.id != id {
        bail!(
            "Herdr API response id mismatch: expected {id:?}, got {:?}",
            response.id
        );
    }
    if let Some(error) = response.error {
        bail!("Herdr API {}: {}", error.code, error.message);
    }
    response.result.context("Herdr API response omitted result")
}

#[cfg(not(unix))]
pub(crate) fn request_socket(_socket: &str, _method: &str, _params: &Value) -> Result<Value> {
    bail!("Herdr's public API socket requires a Unix host")
}
