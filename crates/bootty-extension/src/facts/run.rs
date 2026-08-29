use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How `bootty.run` treats the shared shell-out cache during the current phase.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunMode {
    /// Outside a render (e.g. an `on_reorder` mutation): always shell out, never cache. Keeps
    /// side-effecting commands like `tmux move-window` out of the cache and always executed.
    ///
    /// Currently unreachable: nothing sets it, so every caller lands on `Refresh` or `Cached`. A
    /// module that shells out for effect from an action handler therefore gets the cached path and
    /// runs at most once per refresh gap, not once per action. No built-in does, so this is a
    /// limit rather than a live bug — wire this mode up at the render/action boundary before
    /// documenting `bootty.run` as safe for side effects.
    Live = 1,
    /// Interval render: return the last cached value immediately and refresh it in the background.
    Refresh = 0,
    /// Forced render (a reorder, structural mux change, or completed background refresh): serve
    /// cached output only so the render is instant and side-effect free.
    Cached = 2,
}

/// A command a module asked for: either shell text or an argument vector run directly.
enum RunCommand {
    Shell(String),
    Exec(Vec<String>),
}

impl RunCommand {
    /// Cache identity. Shell text keys on itself, so seeded preview output still matches. An argv
    /// joins on a separator no argument carries, keeping `{"a b"}` and `{"a", "b"}` apart.
    fn cache_key(&self) -> Cow<'_, str> {
        match self {
            Self::Shell(cmd) => Cow::Borrowed(cmd),
            Self::Exec(argv) => Cow::Owned(format!("exec\u{1f}{}", argv.join("\u{1f}"))),
        }
    }

    fn output(&self, run_jobs: &PlatformRunJobs, shutdown: &AtomicBool) -> std::io::Result<String> {
        match self {
            Self::Shell(cmd) => shell_run_output(cmd, run_jobs, shutdown),
            Self::Exec(argv) => exec_run_output(argv, run_jobs, shutdown),
        }
    }
}

/// Caches `bootty.run` query output across renders and refreshes shell-outs off the extension
/// worker so one slow provider/command cannot block unrelated modules.
#[derive(Default)]
pub(super) struct RunCache {
    /// Keyed by the command text, and never evicted: a module's set of commands is fixed by its
    /// source, so this holds one entry per distinct command for the life of the host. A module
    /// that builds a command per session (a path or id interpolated into it) would grow this
    /// without bound — add eviction when one does.
    entries: Mutex<HashMap<String, RunEntry>>,
    /// Current behavior, a `RunMode` discriminant; defaults to `Refresh`.
    mode: AtomicU8,
    /// Shortest gap between two runs of the same command, in milliseconds. Set to the interval of
    /// the surface being rendered, so a module that asks to refresh every 60s shells out every 60s
    /// however often it is re-rendered. Zero (the default) leaves a refresh unthrottled, which is
    /// what a non-render caller wants.
    refresh_gap_ms: AtomicU64,
    run_jobs: Arc<PlatformRunJobs>,
    shutdown: Arc<AtomicBool>,
    /// Branch a settings preview should show. Previews render against example sessions whose paths
    /// do not exist, so a real `HEAD` read has nothing to find.
    pub(super) preview_branch: Option<String>,
}

impl Drop for RunCache {
    fn drop(&mut self) {
        self.retire();
    }
}

#[derive(Default)]
struct RunEntry {
    output: String,
    refreshing: bool,
    /// When the last refresh finished. `None` until the first one lands, so a command a module has
    /// never asked for still runs on its first render.
    refreshed_at: Option<Instant>,
}

impl RunCache {
    pub(super) fn retire(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.run_jobs.cleanup();
    }

    /// Rate-limit refreshes to one per `gap` per command. Callers set this to the interval of the
    /// surface about to render.
    pub(super) fn set_refresh_gap(&self, gap: Duration) {
        self.refresh_gap_ms.store(
            u64::try_from(gap.as_millis()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    fn refresh_gap(&self) -> Duration {
        Duration::from_millis(self.refresh_gap_ms.load(Ordering::Relaxed))
    }

    fn set_mode(&self, mode: RunMode) {
        self.mode.store(mode as u8, Ordering::Relaxed);
    }

    fn mode(&self) -> RunMode {
        match self.mode.load(Ordering::Relaxed) {
            x if x == RunMode::Cached as u8 => RunMode::Cached,
            x if x == RunMode::Refresh as u8 => RunMode::Refresh,
            _ => RunMode::Live,
        }
    }

    pub(super) fn run(self: &Arc<Self>, cmd: &str) -> std::io::Result<(String, bool)> {
        self.run_command(RunCommand::Shell(cmd.to_owned()))
    }

    pub(super) fn exec(self: &Arc<Self>, argv: Vec<String>) -> std::io::Result<(String, bool)> {
        self.run_command(RunCommand::Exec(argv))
    }

    /// What a command last printed, without running it. This is how a module shows an answer the
    /// moment it lands: the command that produces it is started on its own schedule, and every
    /// render in between reads the result for free.
    pub(super) fn read(&self, argv: Vec<String>) -> (String, bool) {
        let cached = self.cached(&RunCommand::Exec(argv).cache_key());
        (cached.clone().unwrap_or_default(), cached.is_some())
    }

    /// Returns the command's output and whether that output is an answer yet. During a render the
    /// first ask for a command only starts it, and an empty string is what a module gets back —
    /// indistinguishable from a command that legitimately printed nothing. The flag is that
    /// difference, so a module can ask again shortly instead of showing nothing until its next
    /// turn.
    fn run_command(self: &Arc<Self>, command: RunCommand) -> std::io::Result<(String, bool)> {
        match self.mode() {
            RunMode::Live => command
                .output(&self.run_jobs, &self.shutdown)
                .map(|output| (output.trim().to_owned(), true)),
            RunMode::Cached => {
                let cached = self.cached(&command.cache_key());
                Ok((cached.clone().unwrap_or_default(), cached.is_some()))
            }
            RunMode::Refresh => {
                let cached = self.cached(&command.cache_key());
                self.refresh(command);
                Ok((cached.clone().unwrap_or_default(), cached.is_some()))
            }
        }
    }

    fn cached(&self, key: &str) -> Option<String> {
        self.entries
            .lock()
            .ok()
            .and_then(|entries| entries.get(key).map(|entry| entry.output.clone()))
    }

    fn refresh(self: &Arc<Self>, command: RunCommand) {
        let key = command.cache_key().into_owned();
        {
            let Ok(mut entries) = self.entries.lock() else {
                return;
            };
            let entry = entries.entry(key.clone()).or_default();
            if entry.refreshing {
                return;
            }
            // Without this a refresh starts the moment the previous run exits, so a command that
            // takes 1.8s re-runs every 1.8s for as long as the module is on screen, whatever
            // interval it asked for. Measured from the last run finishing, so a slow command backs
            // itself off rather than queueing.
            if entry
                .refreshed_at
                .is_some_and(|at| at.elapsed() < self.refresh_gap())
            {
                return;
            }
            entry.refreshing = true;
        }

        let cache = Arc::clone(self);
        std::thread::spawn(move || {
            let output = command
                .output(&cache.run_jobs, &cache.shutdown)
                .map_or_else(
                    |error| format!("bootty.run: {error}"),
                    |output| output.trim().to_owned(),
                );
            if let Ok(mut entries) = cache.entries.lock() {
                let entry = entries.entry(key).or_default();
                entry.output = output;
                entry.refreshing = false;
                entry.refreshed_at = Some(Instant::now());
            }
        });
    }
}

pub(super) fn preview_run_cache() -> Arc<RunCache> {
    let mut cache = RunCache::default();
    cache.preview_branch = Some("feature/module-previews".to_owned());
    let cache = Arc::new(cache);
    cache.set_mode(RunMode::Cached);
    // Example output for the commands the built-ins query, so each one previews with something
    // representative instead of rendering nothing. Add an entry when a built-in learns a new query.
    let usage = |used_primary: u32, used_secondary: u32| {
        format!(
            r#"[{{"usage":{{"primary":{{"usedPercent":{used_primary},"windowMinutes":300}},"secondary":{{"usedPercent":{used_secondary},"windowMinutes":10080}}}}}}]"#
        )
    };
    let commands = [
        (
            RunCommand::Exec(
                [
                    "git",
                    "-C",
                    "/Users/demo/src/bootty",
                    "diff",
                    "HEAD",
                    "--numstat",
                ]
                .map(str::to_owned)
                .to_vec(),
            ),
            "12\t3\tcrates/bootty-app/src/ui/settings/surface.rs".to_owned(),
        ),
        (
            RunCommand::Exec(
                ["codexbar", "usage", "--json", "--provider", "codex"]
                    .map(str::to_owned)
                    .to_vec(),
            ),
            usage(42, 68),
        ),
        (
            RunCommand::Exec(
                ["codexbar", "usage", "--json", "--provider", "claude"]
                    .map(str::to_owned)
                    .to_vec(),
            ),
            usage(17, 31),
        ),
    ];
    if let Ok(mut entries) = cache.entries.lock() {
        for (command, output) in commands {
            entries.insert(
                command.cache_key().into_owned(),
                RunEntry {
                    output: output.clone(),
                    refreshing: false,
                    refreshed_at: None,
                },
            );
        }
    }
    cache
}

static RUN_JOB_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The shells started by in-flight `bootty.run` calls, keyed by job id.
///
/// A module renders on a worker thread that blocks until its command finishes, and dropping the
/// host must not join that thread, so cancellation kills the shell the worker is waiting on: the
/// command's pipe reaches EOF and the worker returns.
#[derive(Default)]
struct PlatformRunJobs {
    children: Mutex<BTreeMap<u64, Child>>,
}

impl PlatformRunJobs {
    fn register(&self, id: u64, child: Child, shutdown: &AtomicBool) -> std::io::Result<()> {
        let mut children = self
            .children
            .lock()
            .map_err(|_| std::io::Error::other("extension run jobs poisoned"))?;
        children.insert(id, child);
        // Drop can set shutdown and clean the registry between spawn and registration. Rechecking
        // while the registry is locked closes that gap: either cleanup sees this child, or this
        // path removes it itself.
        if shutdown.load(Ordering::Acquire) {
            let mut child = children.remove(&id).expect("registered child");
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::other("extension host stopped"));
        }
        Ok(())
    }

    /// Reclaim a finished job. `None` means [`Self::cleanup`] already killed it.
    fn take(&self, id: u64) -> Option<Child> {
        self.children.lock().ok()?.remove(&id)
    }

    fn cleanup(&self) {
        let Ok(mut children) = self.children.lock() else {
            return;
        };
        // ponytail: killing the shell orphans any grandchild it started; a process-group kill
        // needs `libc::killpg`, which the workspace's `unsafe_code = "deny"` rules out.
        for (_, mut child) in std::mem::take(&mut *children) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Run `cmd` through the platform shell and return its merged stdout/stderr.
///
/// The shell inherits Bootty's environment, which `shell_env::hydrate_from_login_shell` already
/// filled with the login shell's PATH at startup, so commands resolve the same tools the user's
/// terminal does.
fn shell_run_output(
    cmd: &str,
    run_jobs: &PlatformRunJobs,
    shutdown: &AtomicBool,
) -> std::io::Result<String> {
    // One pipe for both streams: a module's text keeps the interleaved output the old
    // single-file capture produced, and reading a single end cannot deadlock on a full buffer.
    run_output(shell_command(cmd), true, run_jobs, shutdown)
}

/// Run `argv` directly and return its stdout, leaving the platform shell out of it.
///
/// A module that needs no shell syntax — no pipes, no globbing, no redirects — spends two processes
/// per call going through one, and every argument has to survive quoting on the way. `argv` reaches
/// the program as written, and only the program is spawned. Errors go to the null device, matching
/// the `2>/dev/null` these call sites already asked the shell for.
fn exec_run_output(
    argv: &[String],
    run_jobs: &PlatformRunJobs,
    shutdown: &AtomicBool,
) -> std::io::Result<String> {
    let (program, arguments) = argv
        .split_first()
        .ok_or_else(|| std::io::Error::other("bootty.exec needs a program to run"))?;
    let mut command = Command::new(program);
    command.args(arguments);
    run_output(command, false, run_jobs, shutdown)
}

fn run_output(
    mut command: Command,
    capture_stderr: bool,
    run_jobs: &PlatformRunJobs,
    shutdown: &AtomicBool,
) -> std::io::Result<String> {
    if shutdown.load(Ordering::Acquire) {
        return Err(std::io::Error::other("extension host stopped"));
    }
    let (mut reader, writer) = std::io::pipe()?;
    command.stdin(Stdio::null());
    if capture_stderr {
        command.stderr(writer.try_clone()?);
    } else {
        command.stderr(Stdio::null());
    }
    command.stdout(writer);

    let id = RUN_JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
    let child = command.spawn()?;
    // `command` holds the pipe's write end until it is dropped, and the read below ends only
    // once every writer is closed.
    drop(command);
    run_jobs.register(id, child, shutdown)?;

    let mut output = String::new();
    let read = std::io::Read::read_to_string(&mut reader, &mut output);
    let mut child = run_jobs
        .take(id)
        .ok_or_else(|| std::io::Error::other("extension host stopped"))?;
    let _ = child.wait();
    read?;
    Ok(output)
}

#[cfg(not(windows))]
fn shell_command(cmd: &str) -> Command {
    let mut command = Command::new("sh");
    command.arg("-c").arg(cmd);
    command
}

#[cfg(windows)]
fn shell_command(cmd: &str) -> Command {
    use std::os::windows::process::CommandExt;

    let mut command = Command::new("cmd");
    command
        .creation_flags(windows_no_window_flag())
        .raw_arg(format!("/S /C {cmd}"));
    command
}

#[cfg(windows)]
pub(super) fn platform_shell_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

#[cfg(not(windows))]
pub(super) fn platform_shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(windows)]
const fn windows_no_window_flag() -> u32 {
    0x0800_0000
}
