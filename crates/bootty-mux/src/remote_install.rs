use std::{
    fs::{self, OpenOptions},
    io::{Read as _, Write as _},
    path::PathBuf,
    sync::Mutex,
};

use anyhow::{Context, Result, bail};
use sha2::{Digest as _, Sha256};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

use crate::{process::CommandRunner, ssh::SshRemote, tmux_protocol::shell_quote};

const REPOSITORY_OWNER: &str = "majindotboo";
const REPOSITORY_NAME: &str = "bootty";
fn remote_daemon_path() -> &'static str {
    crate::remote_exec::REMOTE_EXEC_PROGRAM
        .strip_prefix("./")
        .unwrap_or(crate::remote_exec::REMOTE_EXEC_PROGRAM)
}
const REMOTE_DAEMON_DIRECTORY: &str = ".bootty/bin";

static INSTALL_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemoteTarget {
    LinuxX64,
    LinuxArm64,
    MacosX64,
    MacosArm64,
    WindowsX64,
}

impl RemoteTarget {
    fn triple(self) -> &'static str {
        match self {
            Self::LinuxX64 => "x86_64-unknown-linux-gnu",
            Self::LinuxArm64 => "aarch64-unknown-linux-gnu",
            Self::MacosX64 => "x86_64-apple-darwin",
            Self::MacosArm64 => "aarch64-apple-darwin",
            Self::WindowsX64 => "x86_64-pc-windows-msvc",
        }
    }

    fn asset_name(self) -> String {
        format!("bootty-daemon-{}", self.triple())
    }

    fn is_unix(self) -> bool {
        self != Self::WindowsX64
    }

    fn current() -> Option<Self> {
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", "x86_64") => Some(Self::LinuxX64),
            ("linux", "aarch64") => Some(Self::LinuxArm64),
            ("macos", "x86_64") => Some(Self::MacosX64),
            ("macos", "aarch64") => Some(Self::MacosArm64),
            ("windows", "x86_64") => Some(Self::WindowsX64),
            _ => None,
        }
    }
}

trait ArtifactProvider {
    fn daemon(&self, target: RemoteTarget) -> Result<PathBuf>;
}

struct ReleaseArtifacts;

impl ArtifactProvider for ReleaseArtifacts {
    fn daemon(&self, target: RemoteTarget) -> Result<PathBuf> {
        let executable = std::env::current_exe().context("resolve Bootty executable")?;
        if let Some(daemon) = bundled_daemon(&executable, target) {
            return Ok(daemon);
        }
        let asset = target.asset_name();
        let release = format!("v{}", env!("CARGO_PKG_VERSION"));
        let root = format!(
            "https://github.com/{REPOSITORY_OWNER}/{REPOSITORY_NAME}/releases/download/{release}"
        );
        let checksums = String::from_utf8(download(&format!("{root}/SHA256SUMS"))?)
            .context("decode daemon checksums")?;
        let expected = checksums
            .lines()
            .find_map(|line| {
                let (checksum, name) = line.split_once(char::is_whitespace)?;
                (name.trim_start_matches([' ', '*']) == asset).then_some(checksum)
            })
            .with_context(|| format!("SHA256SUMS has no entry for {asset}"))?;
        let cache = daemon_cache_dir()?.join(env!("CARGO_PKG_VERSION"));
        prepare_private_cache_dir(&cache)?;
        let _cache_lock = lock_cache(&cache)?;
        let path = cache.join(&asset);
        if verified_cached_daemon(&path, expected)? {
            return Ok(path);
        }
        let bytes =
            download(&format!("{root}/{asset}")).with_context(|| format!("download {asset}"))?;
        if checksum(&bytes) != expected {
            bail!("checksum mismatch for {asset}")
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let temporary = path.with_extension(format!("{}-{nonce}.download", std::process::id()));
        write_private_file(&temporary, &bytes)?;
        publish_cached_daemon(&temporary, &path, expected)?;
        Ok(path)
    }
}

fn bundled_daemon(executable: &std::path::Path, target: RemoteTarget) -> Option<PathBuf> {
    let directory = executable.parent()?;
    let asset = target.asset_name();
    let mut candidates = Vec::new();
    if RemoteTarget::current() == Some(target) {
        candidates.push(executable.with_file_name(if cfg!(windows) {
            "bootty-daemon.exe"
        } else {
            "bootty-daemon"
        }));
    }
    candidates.extend([
        directory.join("../Resources/daemons").join(&asset),
        directory.join("../share/bootty/daemons").join(&asset),
        directory.join("daemons").join(&asset),
    ]);
    candidates.into_iter().find(|candidate| candidate.is_file())
}

fn daemon_cache_dir() -> Result<PathBuf> {
    #[cfg(windows)]
    let root = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .context("LOCALAPPDATA is unavailable")?;
    #[cfg(not(windows))]
    let root = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .context("HOME is unavailable")?;
    Ok(root.join("bootty/daemons"))
}

fn verified_cached_daemon(path: &std::path::Path, expected: &str) -> Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }
    let cached = fs::read(path).context("read cached daemon")?;
    if checksum(&cached) == expected {
        return Ok(true);
    }
    fs::remove_file(path).context("remove invalid cached daemon")?;
    Ok(false)
}

fn prepare_private_cache_dir(path: &std::path::Path) -> Result<()> {
    fs::create_dir_all(path).context("create daemon download cache")?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .context("make daemon download cache private")?;
    Ok(())
}

fn lock_cache(path: &std::path::Path) -> Result<fs::File> {
    let path = path.join(".lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(&path).context("open daemon cache lock")?;
    #[cfg(unix)]
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .context("make daemon cache lock private")?;
    file.lock().context("lock daemon download cache")?;
    Ok(file)
}

fn write_private_file(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).context("create downloaded daemon")?;
    file.write_all(bytes).context("write downloaded daemon")
}

fn publish_cached_daemon(
    temporary: &std::path::Path,
    path: &std::path::Path,
    expected: &str,
) -> Result<()> {
    let publish = fs::hard_link(temporary, path);
    let _ = fs::remove_file(temporary);
    if let Err(error) = publish
        && !path.is_file()
    {
        return Err(error).context("commit downloaded daemon");
    }
    if !verified_cached_daemon(path, expected)? {
        bail!("daemon download produced an invalid cache entry")
    }
    Ok(())
}

fn checksum(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn download(url: &str) -> Result<Vec<u8>> {
    let mut response = ureq::get(url).call()?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .read_to_end(&mut bytes)
        .context("read download")?;
    Ok(bytes)
}

pub(crate) fn ensure<R: CommandRunner>(remote: &SshRemote, runner: &R) -> Result<()> {
    ensure_with(remote, runner, &ReleaseArtifacts)
}

fn ensure_with<R: CommandRunner, A: ArtifactProvider>(
    remote: &SshRemote,
    runner: &R,
    artifacts: &A,
) -> Result<()> {
    let (program, args) = remote.ping_command();
    if daemon_matches(&runner.run(&program, &args)?) {
        return Ok(());
    }

    let _install = INSTALL_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("remote daemon installation lock is poisoned"))?;
    let (program, args) = remote.ping_command();
    if daemon_matches(&runner.run(&program, &args)?) {
        return Ok(());
    }

    let target = detect_target(remote, runner)?;
    let daemon = artifacts.daemon(target)?;
    let installed = remote_daemon_path();
    let mut generation = [0_u8; 16];
    getrandom::fill(&mut generation).context("allocate remote daemon candidate generation")?;
    let generation = u128::from_ne_bytes(generation);
    let temporary = format!(
        "{installed}.{}-{generation:032x}.upload",
        std::process::id()
    );
    let create = if target.is_unix() {
        format!("mkdir -p {REMOTE_DAEMON_DIRECTORY}")
    } else {
        "cmd.exe /d /s /c \"if not exist .bootty\\bin mkdir .bootty\\bin\"".to_owned()
    };
    require_remote_success(remote, runner, &create, "create remote daemon directory")?;

    let (program, args) = remote.scp_command(&daemon, &temporary);
    match runner.run(&program, &args) {
        Ok(output) if output.success => {}
        Ok(output) => {
            remove_remote_candidate(target, remote, runner, &temporary);
            bail!("upload Bootty daemon: {}", first_error(&output.stderr))
        }
        Err(error) => {
            remove_remote_candidate(target, remote, runner, &temporary);
            return Err(error).context("upload Bootty daemon");
        }
    }
    if target.is_unix()
        && let Err(error) = require_remote_success(
            remote,
            runner,
            &format!("chmod 700 {temporary}"),
            "make remote daemon executable",
        )
    {
        remove_remote_candidate(target, remote, runner, &temporary);
        return Err(error);
    }

    let candidate = if target.is_unix() {
        candidate_ping(remote, runner, &temporary)
    } else {
        Ok(())
    };
    if let Err(error) = candidate {
        remove_remote_candidate(target, remote, runner, &temporary);
        return Err(error);
    }
    let promote = if target.is_unix() {
        unix_publish_command(&temporary, installed)
    } else {
        format!(
            "cmd.exe /d /s /c \"move /-y {} {} <nul >nul\"",
            temporary.replace('/', "\\"),
            installed.replace('/', "\\")
        )
    };
    let (program, args) = remote.raw_command(&promote);
    let output = match runner.run(&program, &args) {
        Ok(output) => output,
        Err(error) => {
            remove_remote_candidate(target, remote, runner, &temporary);
            return Err(error).context("publish Bootty daemon");
        }
    };
    if !output.success {
        remove_remote_candidate(target, remote, runner, &temporary);
        let (program, args) = remote.ping_command();
        if !daemon_matches(&runner.run(&program, &args)?) {
            bail!("install Bootty daemon: {}", first_error(&output.stderr))
        }
        return Ok(());
    }

    let (program, args) = remote.ping_command();
    let output = runner.run(&program, &args)?;
    if !daemon_matches(&output) {
        bail!(
            "Bootty daemon installation on {} did not start with protocol {}: {}",
            remote.host(),
            crate::ssh::REMOTE_DAEMON_PROTOCOL_VERSION,
            first_error(&output.stderr)
        )
    }
    Ok(())
}

fn unix_publish_command(temporary: &str, installed: &str) -> String {
    format!(
        "mv -f {} {}",
        shell_quote(temporary),
        shell_quote(installed)
    )
}

fn candidate_ping<R: CommandRunner>(remote: &SshRemote, runner: &R, temporary: &str) -> Result<()> {
    let command = format!("{} remote-ping", shell_quote(&format!("./{temporary}")));
    let (program, args) = remote.raw_command(&command);
    let output = runner.run(&program, &args)?;
    if daemon_matches(&output) {
        return Ok(());
    }
    bail!(
        "uploaded Bootty daemon on {} did not start with protocol {}: {}",
        remote.host(),
        crate::ssh::REMOTE_DAEMON_PROTOCOL_VERSION,
        first_error(&output.stderr)
    )
}

fn remove_remote_candidate<R: CommandRunner>(
    target: RemoteTarget,
    remote: &SshRemote,
    runner: &R,
    temporary: &str,
) {
    if !target.is_unix() {
        return;
    }
    let command = format!("rm -f {}", shell_quote(temporary));
    let (program, args) = remote.raw_command(&command);
    let _ = runner.run(&program, &args);
}

fn daemon_matches(output: &crate::process::CommandOutput) -> bool {
    output.success
        && output.stdout.trim()
            == format!(
                "{}:{}",
                crate::ssh::REMOTE_DAEMON_PROTOCOL_VERSION,
                env!("CARGO_PKG_VERSION")
            )
}

fn detect_target<R: CommandRunner>(remote: &SshRemote, runner: &R) -> Result<RemoteTarget> {
    let (program, args) = remote.raw_command("uname -s && uname -m");
    let output = runner.run(&program, &args)?;
    if output.success {
        let mut lines = output.stdout.lines().map(str::trim);
        return match (lines.next(), lines.next()) {
            (Some("Linux"), Some("x86_64" | "amd64")) => Ok(RemoteTarget::LinuxX64),
            (Some("Linux"), Some("aarch64" | "arm64")) => Ok(RemoteTarget::LinuxArm64),
            (Some("Darwin"), Some("x86_64" | "amd64")) => Ok(RemoteTarget::MacosX64),
            (Some("Darwin"), Some("arm64" | "aarch64")) => Ok(RemoteTarget::MacosArm64),
            (os, architecture) => bail!(
                "unsupported remote platform {} {}",
                os.unwrap_or("unknown"),
                architecture.unwrap_or("unknown")
            ),
        };
    }

    let (program, args) =
        remote.raw_command("cmd.exe /d /s /c \"echo Windows&&echo %PROCESSOR_ARCHITECTURE%\"");
    let output = runner.run(&program, &args)?;
    if output.success
        && output
            .stdout
            .lines()
            .any(|line| line.trim().eq_ignore_ascii_case("AMD64"))
    {
        return Ok(RemoteTarget::WindowsX64);
    }
    bail!(
        "could not detect a supported operating system on {}",
        remote.host()
    )
}

fn require_remote_success<R: CommandRunner>(
    remote: &SshRemote,
    runner: &R,
    command: &str,
    operation: &str,
) -> Result<()> {
    let (program, args) = remote.raw_command(command);
    let output = runner.run(&program, &args)?;
    if output.success {
        return Ok(());
    }
    bail!("{operation}: {}", first_error(&output.stderr))
}

fn first_error(detail: &str) -> &str {
    detail
        .lines()
        .next()
        .filter(|line| !line.is_empty())
        .unwrap_or("command failed")
}
