use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use bootty_mux_model::SshTarget;
use bootty_remote::ssh::SshRemote;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    control::{HerdrApi, parse_snapshot_value, request_socket},
    model::HerdrSessionSnapshot,
};

const REMOTE_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const REMOTE_READY_POLL: Duration = Duration::from_millis(100);

static DIRECTORY_ID: AtomicU64 = AtomicU64::new(1);
static BRIDGES: OnceLock<Mutex<HashMap<String, Weak<RemoteHerdrBridge>>>> = OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteHerdrBridgePlan {
    pub program: String,
    pub arguments: Vec<String>,
    pub local_socket: PathBuf,
    pub local_client_socket: PathBuf,
    pub remote_socket: PathBuf,
    pub remote_client_socket: PathBuf,
}

impl RemoteHerdrBridgePlan {
    pub fn new(target: &SshTarget, directory: &Path, remote_socket: &Path) -> Result<Self> {
        validate_target(target)?;
        validate_absolute_path(directory, "local bridge directory")?;
        validate_absolute_path(remote_socket, "remote Herdr socket")?;
        let remote_parent = remote_socket
            .parent()
            .context("remote Herdr socket has no parent directory")?;
        let remote_client_socket = remote_parent.join("herdr-client.sock");
        let local_socket = directory.join("herdr.sock");
        let local_client_socket = directory.join("herdr-client.sock");
        validate_absolute_path(&remote_client_socket, "remote Herdr client socket")?;
        validate_local_socket_length(&local_socket)?;
        validate_local_socket_length(&local_client_socket)?;
        let local_socket_text = path_text(&local_socket, "local Herdr socket")?;
        let local_client_socket_text =
            path_text(&local_client_socket, "local Herdr client socket")?;
        let remote_socket_text = path_text(remote_socket, "remote Herdr socket")?;
        let remote_client_socket_text =
            path_text(&remote_client_socket, "remote Herdr client socket")?;

        let remote = SshRemote::new(target.clone());
        let (program, mut arguments) = remote.command("true", &[]);
        arguments
            .pop()
            .context("SSH command omitted remote command")?;
        let option_end = arguments
            .iter()
            .position(|argument| argument == "--")
            .context("SSH command omitted option terminator")?;
        let forwards = vec![
            "-o".to_owned(),
            "ControlMaster=no".to_owned(),
            "-o".to_owned(),
            "ControlPath=none".to_owned(),
            "-o".to_owned(),
            "ExitOnForwardFailure=yes".to_owned(),
            "-o".to_owned(),
            "StreamLocalBindUnlink=yes".to_owned(),
            "-L".to_owned(),
            format!("{local_socket_text}:{remote_socket_text}"),
            "-L".to_owned(),
            format!("{local_client_socket_text}:{remote_client_socket_text}"),
            "-N".to_owned(),
        ];
        arguments.splice(0..0, forwards[..4].iter().cloned());
        arguments.splice(
            option_end + 4..option_end + 4,
            forwards[4..].iter().cloned(),
        );

        Ok(Self {
            program,
            arguments,
            local_socket,
            local_client_socket,
            remote_socket: remote_socket.to_owned(),
            remote_client_socket,
        })
    }

    pub fn server_bootstrap_command(
        target: &SshTarget,
        session: &str,
    ) -> Result<(String, Vec<String>)> {
        validate_target(target)?;
        validate_session(session)?;
        let script = concat!(
            "nohup herdr --session \"$1\" server </dev/null >/dev/null 2>&1 & ",
            "attempt=0; while [ \"$attempt\" -lt 80 ]; do ",
            "herdr --session \"$1\" api snapshot >/dev/null 2>&1 && exit 0; ",
            "attempt=$((attempt + 1)); sleep 0.1; done; exit 1"
        );
        let args = vec![
            "-c".to_owned(),
            script.to_owned(),
            "bootty-herdr".to_owned(),
            session.to_owned(),
        ];
        Ok(SshRemote::new(target.clone()).command("sh", &args))
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct RemoteHerdrStatus {
    pub running: bool,
    pub socket: Option<PathBuf>,
}

#[derive(Deserialize)]
struct RemoteStatusEnvelope {
    server: RemoteHerdrStatus,
}

pub fn parse_remote_status(stdout: &str) -> Result<RemoteHerdrStatus> {
    let status: RemoteStatusEnvelope =
        serde_json::from_str(stdout).context("decode remote `herdr status --json`")?;
    if status.server.running {
        let socket = status
            .server
            .socket
            .as_deref()
            .context("running remote Herdr server omitted its socket")?;
        validate_absolute_path(socket, "remote Herdr socket")?;
    }
    Ok(status.server)
}

pub fn remote_status_command(target: &SshTarget, session: &str) -> Result<(String, Vec<String>)> {
    validate_target(target)?;
    validate_session(session)?;
    let args = vec![
        "--session".to_owned(),
        session.to_owned(),
        "status".to_owned(),
        "--json".to_owned(),
    ];
    Ok(SshRemote::new(target.clone()).command("herdr", &args))
}

struct BridgeState {
    directory: PathBuf,
    tunnel: Option<Child>,
}

pub(crate) struct RemoteHerdrBridge {
    target: SshTarget,
    session: String,
    state: Mutex<BridgeState>,
}

impl RemoteHerdrBridge {
    pub(crate) fn shared(target: SshTarget, session: String) -> Result<Arc<Self>> {
        validate_target(&target)?;
        validate_session(&session)?;
        let key = bridge_key(&target, &session);
        let bridges = BRIDGES.get_or_init(|| Mutex::new(HashMap::new()));
        let mut bridges = bridges
            .lock()
            .map_err(|_| anyhow::anyhow!("remote Herdr bridge registry lock poisoned"))?;
        if let Some(bridge) = bridges.get(&key).and_then(Weak::upgrade) {
            return Ok(bridge);
        }
        let directory = private_directory()?;
        let bridge = Arc::new(Self {
            target,
            session,
            state: Mutex::new(BridgeState {
                directory,
                tunnel: None,
            }),
        });
        bridges.insert(key, Arc::downgrade(&bridge));
        Ok(bridge)
    }

    pub(crate) fn target(&self) -> &SshTarget {
        &self.target
    }

    pub(crate) fn socket_path(&self) -> Result<PathBuf> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("remote Herdr bridge lock poisoned"))?;
        let existing_socket = state.directory.join("herdr.sock");
        if state
            .tunnel
            .as_mut()
            .is_some_and(|child| child.try_wait().ok().flatten().is_none())
            && existing_socket.exists()
            && state.directory.join("herdr-client.sock").exists()
            && request_socket(
                path_text(&existing_socket, "local Herdr socket")?,
                "ping",
                &json!({}),
            )
            .is_ok()
        {
            return Ok(existing_socket);
        }
        stop_tunnel(&mut state);
        remove_socket_nodes(&state.directory);
        let status = self.ensure_remote_server()?;
        let remote_socket = status
            .socket
            .context("remote Herdr status omitted socket")?;
        let plan = RemoteHerdrBridgePlan::new(&self.target, &state.directory, &remote_socket)?;
        let child = Command::new(&plan.program)
            .args(&plan.arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("start Herdr SSH tunnel to {}", self.target.host))?;
        state.tunnel = Some(child);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if plan.local_socket.exists() && plan.local_client_socket.exists() {
                let socket = path_text(&plan.local_socket, "local Herdr socket")?;
                if request_socket(socket, "ping", &json!({})).is_ok() {
                    return Ok(plan.local_socket);
                }
            }
            if state
                .tunnel
                .as_mut()
                .is_some_and(|child| child.try_wait().ok().flatten().is_some())
            {
                stop_tunnel(&mut state);
                bail!(
                    "Herdr SSH tunnel to {} exited during startup",
                    self.target.host
                )
            }
            thread::sleep(Duration::from_millis(20));
        }
        stop_tunnel(&mut state);
        bail!("timed out waiting for Herdr SSH socket forwards")
    }

    fn remote_status(&self) -> Result<RemoteHerdrStatus> {
        let (program, args) = remote_status_command(&self.target, &self.session)?;
        let output = command_output_timeout(&program, &args, REMOTE_COMMAND_TIMEOUT)
            .with_context(|| format!("query Herdr on {}", self.target.host))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("remote Herdr status failed: {}", stderr.trim())
        }
        parse_remote_status(
            &String::from_utf8(output.stdout).context("remote Herdr status was not UTF-8")?,
        )
    }

    fn ensure_remote_server(&self) -> Result<RemoteHerdrStatus> {
        let status = self.remote_status()?;
        if status.running {
            return Ok(status);
        }
        let (program, args) =
            RemoteHerdrBridgePlan::server_bootstrap_command(&self.target, &self.session)?;
        let output = command_output_timeout(&program, &args, REMOTE_COMMAND_TIMEOUT)
            .with_context(|| format!("start Herdr on {}", self.target.host))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("remote Herdr bootstrap failed: {}", stderr.trim())
        }
        let deadline = Instant::now() + REMOTE_COMMAND_TIMEOUT;
        while Instant::now() < deadline {
            let status = self.remote_status()?;
            if status.running {
                return Ok(status);
            }
            thread::sleep(REMOTE_READY_POLL);
        }
        bail!(
            "remote Herdr session {:?} did not become ready within {} seconds",
            self.session,
            REMOTE_COMMAND_TIMEOUT.as_secs()
        )
    }
}

impl Drop for RemoteHerdrBridge {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            stop_tunnel(&mut state);
            remove_socket_nodes(&state.directory);
            let _ = fs::remove_dir(&state.directory);
        }
    }
}

#[derive(Clone)]
pub(crate) struct RemoteHerdrApi {
    bridge: Arc<RemoteHerdrBridge>,
}

impl RemoteHerdrApi {
    pub(crate) fn new(bridge: Arc<RemoteHerdrBridge>) -> Self {
        Self { bridge }
    }
}

impl HerdrApi for RemoteHerdrApi {
    fn snapshot(&self) -> Result<HerdrSessionSnapshot> {
        parse_snapshot_value(&self.request("session.snapshot", json!({}))?)
    }

    fn request(&self, method: &str, params: Value) -> Result<Value> {
        let socket = self.bridge.socket_path()?;
        request_socket(&socket.to_string_lossy(), method, &params)
    }
}

fn validate_target(target: &SshTarget) -> Result<()> {
    if target.host.trim().is_empty() || target.host.trim() != target.host {
        bail!("remote Herdr target host is empty")
    }
    if invalid_process_argument(&target.host) {
        bail!("remote Herdr target host contains an invalid character")
    }
    if target.user.as_deref().is_some_and(|user| {
        user.is_empty() || user.trim() != user || invalid_process_argument(user)
    }) {
        bail!("remote Herdr target user is invalid")
    }
    if target.program.is_empty() || invalid_process_argument(&target.program) {
        bail!("remote Herdr SSH program is invalid")
    }
    if target
        .args
        .iter()
        .any(|argument| invalid_process_argument(argument))
    {
        bail!("remote Herdr SSH argument contains an invalid character")
    }
    Ok(())
}

fn invalid_process_argument(value: &str) -> bool {
    value.bytes().any(|byte| matches!(byte, 0 | b'\n' | b'\r'))
}

fn validate_session(session: &str) -> Result<()> {
    if session.is_empty()
        || !session
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        bail!("Herdr session must contain only letters, digits, '.', '-', or '_'")
    }
    Ok(())
}

fn validate_absolute_path(path: &Path, name: &str) -> Result<()> {
    if !path.is_absolute() || path.as_os_str().is_empty() {
        bail!("{name} must be an absolute path")
    }
    if path_text(path, name)?
        .chars()
        .any(|character| matches!(character, '\n' | '\r' | '\0' | ':'))
    {
        bail!("{name} contains an invalid character")
    }
    Ok(())
}

fn path_text<'a>(path: &'a Path, name: &str) -> Result<&'a str> {
    path.to_str()
        .with_context(|| format!("{name} must be valid UTF-8"))
}

fn validate_local_socket_length(path: &Path) -> Result<()> {
    if path_text(path, "local Herdr socket")?.len() >= 100 {
        bail!("local Herdr SSH socket path is too long")
    }
    Ok(())
}

fn bridge_key(target: &SshTarget, session: &str) -> String {
    format!(
        "{}\0{:?}\0{:?}\0{}\0{:?}\0{session}",
        target.host, target.user, target.port, target.program, target.args
    )
}

fn private_directory() -> Result<PathBuf> {
    for _ in 0..100 {
        let directory = std::env::temp_dir().join(format!(
            "bootty-herdr-{}-{}",
            std::process::id(),
            DIRECTORY_ID.fetch_add(1, Ordering::Relaxed)
        ));
        #[cfg(unix)]
        let result = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700).create(&directory)
        };
        #[cfg(not(unix))]
        let result = fs::create_dir(&directory);
        match result {
            Ok(()) => return Ok(directory),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error).context("create private Herdr bridge directory"),
        }
    }
    bail!("could not allocate private Herdr bridge directory")
}

fn stop_tunnel(state: &mut BridgeState) {
    if let Some(mut child) = state.tunnel.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn remove_socket_nodes(directory: &Path) {
    for name in ["herdr.sock", "herdr-client.sock"] {
        let path = directory.join(name);
        if path.symlink_metadata().is_ok() {
            let _ = fs::remove_file(path);
        }
    }
}

fn command_output_timeout(
    program: &str,
    arguments: &[String],
    timeout: Duration,
) -> Result<std::process::Output> {
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("run {program}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        if child
            .try_wait()
            .with_context(|| format!("wait for {program}"))?
            .is_some()
        {
            return child
                .wait_with_output()
                .with_context(|| format!("collect {program} output"));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{program} timed out after {} seconds", timeout.as_secs())
        }
        thread::sleep(Duration::from_millis(20));
    }
}
