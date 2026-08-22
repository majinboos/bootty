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

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        collections::VecDeque,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use bootty_mux_model::SshTarget;

    use super::*;
    use crate::process::CommandOutput;

    struct Runner {
        outputs: RefCell<VecDeque<CommandOutput>>,
        calls: RefCell<Vec<(String, Vec<String>)>>,
    }

    impl CommandRunner for Runner {
        fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
            self.calls
                .borrow_mut()
                .push((program.to_owned(), args.to_vec()));
            self.outputs
                .borrow_mut()
                .pop_front()
                .context("missing scripted output")
        }
    }

    struct Artifacts {
        path: PathBuf,
    }

    impl ArtifactProvider for Artifacts {
        fn daemon(&self, _target: RemoteTarget) -> Result<PathBuf> {
            Ok(self.path.clone())
        }
    }

    fn output(success: bool, stdout: &str) -> CommandOutput {
        CommandOutput {
            success,
            stdout: stdout.to_owned(),
            stderr: if success {
                String::new()
            } else {
                "not found".to_owned()
            },
        }
    }

    fn ping_output() -> CommandOutput {
        output(
            true,
            &format!(
                "{}:{}",
                crate::ssh::REMOTE_DAEMON_PROTOCOL_VERSION,
                env!("CARGO_PKG_VERSION")
            ),
        )
    }

    #[test]
    fn cached_daemon_must_match_the_release_checksum() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("daemon");
        std::fs::write(&path, b"corrupt").expect("cache");

        assert!(!verified_cached_daemon(&path, &checksum(b"expected")).expect("verify"));
        assert!(!path.exists());

        std::fs::write(&path, b"expected").expect("cache");
        assert!(verified_cached_daemon(&path, &checksum(b"expected")).expect("verify"));
    }

    #[test]
    fn packaged_cross_target_daemon_precedes_release_downloads() {
        let directory = tempfile::tempdir().expect("tempdir");
        let executable = directory.path().join("Bootty.app/Contents/MacOS/bootty");
        let daemon = directory
            .path()
            .join("Bootty.app/Contents/Resources/daemons")
            .join(RemoteTarget::LinuxX64.asset_name());
        std::fs::create_dir_all(executable.parent().expect("executable directory"))
            .expect("executable directory");
        std::fs::create_dir_all(daemon.parent().expect("daemon directory"))
            .expect("daemon directory");
        std::fs::write(&executable, []).expect("executable");
        std::fs::write(&daemon, []).expect("daemon");

        let resolved = bundled_daemon(&executable, RemoteTarget::LinuxX64).expect("bundled daemon");
        assert_eq!(
            std::fs::canonicalize(resolved).expect("resolved daemon"),
            std::fs::canonicalize(daemon).expect("expected daemon")
        );
    }

    #[test]
    fn cache_publication_verifies_an_existing_winner() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("daemon");
        let temporary = directory.path().join("daemon.download");
        std::fs::write(&path, b"expected").expect("winner");
        std::fs::write(&temporary, b"expected").expect("temporary");

        publish_cached_daemon(&temporary, &path, &checksum(b"expected")).expect("publish");

        assert_eq!(std::fs::read(&path).expect("cached daemon"), b"expected");
        assert!(!temporary.exists());
    }

    #[cfg(unix)]
    #[test]
    fn cache_files_are_private() {
        let directory = tempfile::tempdir().expect("tempdir");
        let cache = directory.path().join("cache");
        let daemon = cache.join("daemon");

        prepare_private_cache_dir(&cache).expect("cache");
        write_private_file(&daemon, b"daemon").expect("daemon");
        let _lock = lock_cache(&cache).expect("lock");

        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(&cache)
                    .expect("cache metadata")
                    .permissions()
            ) & 0o777,
            0o700
        );
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(&daemon)
                    .expect("daemon metadata")
                    .permissions()
            ) & 0o777,
            0o600
        );
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(cache.join(".lock"))
                    .expect("lock metadata")
                    .permissions()
            ) & 0o777,
            0o600
        );
    }

    #[test]
    fn separate_remotes_serialize_daemon_installation() {
        struct ConcurrentRunner {
            installed: AtomicBool,
            promotions: AtomicUsize,
        }

        impl CommandRunner for ConcurrentRunner {
            fn run(&self, _program: &str, args: &[String]) -> Result<CommandOutput> {
                let command = args.last().map(String::as_str).unwrap_or_default();
                if command.ends_with("remote-ping") {
                    if command.contains(".upload") {
                        return Ok(ping_output());
                    }
                    return Ok(if self.installed.load(Ordering::SeqCst) {
                        ping_output()
                    } else {
                        output(false, "")
                    });
                }
                if command == "uname -s && uname -m" {
                    return Ok(output(true, "Linux\nx86_64\n"));
                }
                if command.contains("mv -f") {
                    self.promotions.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(20));
                    self.installed.store(true, Ordering::SeqCst);
                }
                Ok(output(true, ""))
            }
        }

        let daemon = tempfile::NamedTempFile::new().expect("daemon");
        let daemon = daemon.path().to_owned();
        let runner = Arc::new(ConcurrentRunner {
            installed: AtomicBool::new(false),
            promotions: AtomicUsize::new(0),
        });
        let handles = (0..4)
            .map(|_| {
                let daemon = daemon.clone();
                let runner = runner.clone();
                std::thread::spawn(move || {
                    ensure_with(
                        &SshRemote::new(SshTarget::for_host("lab")),
                        runner.as_ref(),
                        &Artifacts { path: daemon },
                    )
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().expect("installer thread").expect("install");
        }

        assert_eq!(runner.promotions.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn missing_daemon_is_detected_uploaded_and_verified() {
        let daemon = tempfile::NamedTempFile::new().expect("daemon");
        let runner = Runner {
            outputs: RefCell::new(VecDeque::from([
                output(false, ""),
                output(false, ""),
                output(true, "Linux\nx86_64\n"),
                output(true, ""),
                output(true, ""),
                output(true, ""),
                ping_output(),
                output(true, ""),
                ping_output(),
            ])),
            calls: RefCell::new(Vec::new()),
        };
        let remote = SshRemote::new(SshTarget::for_host("lab"));

        ensure_with(
            &remote,
            &runner,
            &Artifacts {
                path: daemon.path().to_owned(),
            },
        )
        .expect("install daemon");

        let calls = runner.calls.into_inner();
        assert_eq!(calls.len(), 9);
        assert!(calls[4].0.ends_with("scp"));
        assert!(calls[4].1.last().expect("destination").contains(&format!(
            "{}.{}-",
            remote_daemon_path(),
            std::process::id()
        )));
        assert!(
            calls[5]
                .1
                .last()
                .expect("chmod command")
                .contains("chmod 700")
        );
        let candidate_ping = calls[6].1.last().expect("candidate ping command");
        assert!(candidate_ping.contains("remote-ping"));
        assert!(candidate_ping.contains(".upload"));
        let promotion = calls[7].1.last().expect("promotion command");
        assert!(promotion.contains("mv -f"));
        assert!(promotion.contains(remote_daemon_path()));
        assert!(
            calls[8]
                .1
                .last()
                .expect("installed ping")
                .contains("remote-ping")
        );
    }

    #[test]
    fn failed_promotion_accepts_only_a_compatible_winner() {
        let daemon = tempfile::NamedTempFile::new().expect("daemon");
        for (winner, succeeds) in [(ping_output(), true), (output(false, ""), false)] {
            let runner = Runner {
                outputs: RefCell::new(VecDeque::from([
                    output(false, ""),
                    output(false, ""),
                    output(true, "Linux\nx86_64\n"),
                    output(true, ""),
                    output(true, ""),
                    output(true, ""),
                    ping_output(),
                    output(false, ""),
                    output(true, ""),
                    winner,
                ])),
                calls: RefCell::new(Vec::new()),
            };

            let result = ensure_with(
                &SshRemote::new(SshTarget::for_host("lab")),
                &runner,
                &Artifacts {
                    path: daemon.path().to_owned(),
                },
            );

            assert_eq!(result.is_ok(), succeeds);
            assert!(
                runner.calls.borrow()[6]
                    .1
                    .last()
                    .expect("candidate ping")
                    .contains("remote-ping")
            );
            assert!(
                runner.calls.borrow()[7]
                    .1
                    .last()
                    .expect("promotion")
                    .contains("mv -f")
            );
            assert!(
                runner.calls.borrow()[8]
                    .1
                    .last()
                    .expect("candidate cleanup")
                    .contains("rm -f")
            );
            assert!(
                runner.calls.borrow()[9]
                    .1
                    .last()
                    .expect("installed ping")
                    .contains("remote-ping")
            );
        }
    }

    #[test]
    fn incompatible_unix_daemon_is_replaced_after_candidate_ping() {
        let daemon = tempfile::NamedTempFile::new().expect("daemon");
        let runner = Runner {
            outputs: RefCell::new(VecDeque::from([
                output(true, "1:old"),
                output(true, "1:old"),
                output(true, "Linux\nx86_64\n"),
                output(true, ""),
                output(true, ""),
                output(true, ""),
                ping_output(),
                output(true, ""),
                ping_output(),
            ])),
            calls: RefCell::new(Vec::new()),
        };

        ensure_with(
            &SshRemote::new(SshTarget::for_host("lab")),
            &runner,
            &Artifacts {
                path: daemon.path().to_owned(),
            },
        )
        .expect("replace old daemon");

        let calls = runner.calls.into_inner();
        assert!(
            calls[6]
                .1
                .last()
                .expect("candidate ping")
                .contains("remote-ping")
        );
        assert!(
            calls[6]
                .1
                .last()
                .expect("candidate ping")
                .contains(".upload")
        );
        assert!(calls[7].1.last().expect("promotion").contains("mv -f"));
        assert!(
            calls[8]
                .1
                .last()
                .expect("installed ping")
                .contains("remote-ping")
        );
    }

    #[test]
    fn bad_candidate_preserves_old_daemon_and_cleans_candidate() {
        let daemon = tempfile::NamedTempFile::new().expect("daemon");
        let runner = Runner {
            outputs: RefCell::new(VecDeque::from([
                output(true, "1:old"),
                output(true, "1:old"),
                output(true, "Linux\nx86_64\n"),
                output(true, ""),
                output(true, ""),
                output(true, ""),
                output(true, "0:bad"),
                output(true, ""),
            ])),
            calls: RefCell::new(Vec::new()),
        };

        let result = ensure_with(
            &SshRemote::new(SshTarget::for_host("lab")),
            &runner,
            &Artifacts {
                path: daemon.path().to_owned(),
            },
        );

        assert!(result.is_err());
        let calls = runner.calls.into_inner();
        assert_eq!(calls.len(), 8);
        assert!(calls[7].1.last().expect("cleanup").contains("rm -f"));
        assert!(
            !calls
                .iter()
                .any(|(_, args)| { args.last().is_some_and(|command| command.contains("mv -f")) })
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_publication_replaces_the_path_without_changing_the_open_inode() {
        let directory = tempfile::tempdir().expect("tempdir");
        let installed = directory.path().join("installed daemon");
        let candidate = directory.path().join("candidate daemon");
        std::fs::write(&installed, b"old daemon").expect("old daemon");
        std::fs::write(&candidate, b"new daemon").expect("new daemon");
        let mut running_inode = std::fs::File::open(&installed).expect("open old daemon inode");

        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(unix_publish_command(
                candidate.to_str().expect("candidate path"),
                installed.to_str().expect("installed path"),
            ))
            .status()
            .expect("publish daemon");

        assert!(status.success());
        assert_eq!(
            std::fs::read(&installed).expect("installed daemon"),
            b"new daemon"
        );
        let mut old_bytes = Vec::new();
        std::io::Read::read_to_end(&mut running_inode, &mut old_bytes)
            .expect("read old daemon inode");
        assert_eq!(old_bytes, b"old daemon");
    }

    #[test]
    fn windows_probe_selects_the_msvc_daemon() {
        struct TargetArtifacts(RefCell<Option<RemoteTarget>>);
        impl ArtifactProvider for TargetArtifacts {
            fn daemon(&self, target: RemoteTarget) -> Result<PathBuf> {
                self.0.replace(Some(target));
                Ok(PathBuf::from("daemon"))
            }
        }
        let runner = Runner {
            outputs: RefCell::new(VecDeque::from([
                output(false, ""),
                output(false, ""),
                output(false, ""),
                output(true, "Windows\r\nAMD64\r\n"),
                output(true, ""),
                output(true, ""),
                output(true, ""),
                ping_output(),
            ])),
            calls: RefCell::new(Vec::new()),
        };
        let artifacts = TargetArtifacts(RefCell::new(None));

        ensure_with(
            &SshRemote::new(SshTarget::for_host("windows")),
            &runner,
            &artifacts,
        )
        .expect("install daemon");

        assert_eq!(artifacts.0.into_inner(), Some(RemoteTarget::WindowsX64));
        let calls = runner.calls.into_inner();
        assert_eq!(calls.len(), 8);
        assert!(!calls.iter().any(|(_, args)| {
            args.last().is_some_and(|command| {
                command.contains(".upload") && command.contains("remote-ping")
            })
        }));
        let promotion = calls[6].1.last().expect("promotion command");
        assert!(promotion.contains("move /-y"));
        assert!(!promotion.contains("move /y"));
        assert!(
            calls[7]
                .1
                .last()
                .expect("installed ping")
                .contains("remote-ping")
        );
    }
}
