//! Read-only host facts and bounded platform jobs for one extension generation.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

use mlua::{Lua, Table, Value};
use starship_battery::{Manager as BatteryManager, State as BatteryState, units::time::second};
use sysinfo::{MemoryRefreshKind, System};

use crate::extension_ui::{
    Metrics, MuxView, SessionProgressView, SessionReorder, SessionView, WindowView,
};
use crate::{
    command_extensions::{
        ModuleIdentity, WorkerControl,
        processes::{ManagedProcesses, ProcessEvent, ProcessStatus},
    },
    commands::CommandCatalog,
};

/// How long a macOS memory-pressure sample serves every host before another subprocess runs.
#[cfg(target_os = "macos")]
const MEMORY_PRESSURE_TTL: Duration = Duration::from_secs(5);
const EXTENSION_UI_PRELUDE: &str = include_str!("../extension_ui.luau");
const SIDEBAR_FACTS_PRELUDE: &str = include_str!("../sidebar_session_facts.luau");

/// How `bootty.run` treats the shared shell-out cache during the current phase.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunMode {
    /// Outside a render (e.g. an `on_reorder` mutation): always shell out, never cache. Keeps
    /// side-effecting commands like `tmux move-window` out of the cache and always executed.
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
struct RunCache {
    entries: Mutex<HashMap<String, RunEntry>>,
    /// Current behavior, a `RunMode` discriminant; defaults to `Refresh`.
    mode: AtomicU8,
    run_jobs: Arc<PlatformRunJobs>,
    shutdown: Arc<AtomicBool>,
    /// Branch a settings preview should show. Previews render against example sessions whose paths
    /// do not exist, so a real `HEAD` read has nothing to find.
    preview_branch: Option<String>,
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
}

impl RunCache {
    fn retire(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.run_jobs.cleanup();
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

    fn run(self: &Arc<Self>, cmd: &str) -> std::io::Result<(String, bool)> {
        self.run_command(RunCommand::Shell(cmd.to_owned()))
    }

    fn exec(self: &Arc<Self>, argv: Vec<String>) -> std::io::Result<(String, bool)> {
        self.run_command(RunCommand::Exec(argv))
    }

    /// What a command last printed, without running it. This is how a module shows an answer the
    /// moment it lands: the command that produces it is started on its own schedule, and every
    /// render in between reads the result for free.
    fn read(&self, argv: Vec<String>) -> (String, bool) {
        let cached = self.cached(&RunCommand::Exec(argv).cache_key());
        (cached.clone().unwrap_or_default(), cached.is_some())
    }

    /// Returns the command's output and whether that output is an answer yet. During a render the
    /// first ask for a command only starts it, and an empty string is what a module gets back —
    /// indistinguishable from a command that legitimately printed nothing. The flag is that
    /// difference, so a module can ask again shortly instead of showing nothing until its next turn.
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
            entry.refreshing = true;
        }

        let cache = Arc::clone(self);
        std::thread::spawn(move || {
            let output = command
                .output(&cache.run_jobs, &cache.shutdown)
                .map(|output| output.trim().to_owned())
                .unwrap_or_else(|error| format!("bootty.run: {error}"));
            if let Ok(mut entries) = cache.entries.lock() {
                let entry = entries.entry(key).or_default();
                entry.output = output;
                entry.refreshing = false;
            }
        });
    }
}

fn preview_run_cache() -> Arc<RunCache> {
    let mut cache = RunCache::default();
    cache.preview_branch = Some("feature/module-previews".to_owned());
    let cache = Arc::new(cache);
    cache.set_mode(RunMode::Cached);
    let commands = [(
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
        "12\t3\tcrates/bootty-app/src/ui/settings/modules.rs".to_owned(),
    )];
    if let Ok(mut entries) = cache.entries.lock() {
        for (command, output) in commands {
            entries.insert(
                command.cache_key().into_owned(),
                RunEntry {
                    output: output.to_owned(),
                    refreshing: false,
                },
            );
        }
    }
    cache
}

fn preview_mux_view() -> MuxView {
    MuxView {
        scope_key: "preview:binding".to_owned(),
        windows: vec![
            WindowView {
                id: "@1".to_owned(),
                index: 1,
                name: "editor".to_owned(),
                active: true,
                ..WindowView::default()
            },
            WindowView {
                id: "@2".to_owned(),
                index: 2,
                name: "tests".to_owned(),
                progress: Some(62),
                ..WindowView::default()
            },
            WindowView {
                id: "@3".to_owned(),
                index: 3,
                name: "server".to_owned(),
                progress_indeterminate: true,
                ..WindowView::default()
            },
        ],
        sessions: vec![
            SessionView {
                id: "$1".to_owned(),
                name: "work/api".to_owned(),
                display_name: String::new(),
                active: true,
                selected: true,
                cwd: Some("/Users/demo/src/bootty".to_owned()),
                pane_id: Some("%1".to_owned()),
                pane_pid: Some(4242),
                process: Some("cargo test".to_owned()),
                color: Some("#89b4fa".to_owned()),
                dim_color: Some("#585b70".to_owned()),
                progress: Some(62),
                progresses: vec![SessionProgressView {
                    process: "cargo test".to_owned(),
                    value: 62,
                    indeterminate: false,
                }],
                ports: vec![3000, 8080],
                ..SessionView::default()
            },
            SessionView {
                id: "$2".to_owned(),
                name: "work/web".to_owned(),
                display_name: String::new(),
                active: true,
                cwd: Some("/Users/demo/src/web".to_owned()),
                color: Some("#a6e3a1".to_owned()),
                dim_color: Some("#585b70".to_owned()),
                ..SessionView::default()
            },
        ],
        session: Some("work/api".to_owned()),
        sidebar_visible: false,
        session_color: Some("#89b4fa".to_owned()),
        keep_awake: true,
        focused: true,
    }
}

pub(crate) fn sample_metrics(system: &mut System, battery: Option<&BatteryManager>) -> Metrics {
    system.refresh_cpu_usage();
    system.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
    let load = System::load_average();
    let (battery_percent, on_ac, battery_time_to_empty_secs, battery_time_to_full_secs) =
        battery_status(battery);
    Metrics {
        cpu: system.global_cpu_usage(),
        load1: load.one,
        mem_used_pct: memory_used_percent(system),
        mem_total_bytes: system.total_memory(),
        battery_percent,
        on_ac,
        battery_time_to_empty_secs,
        battery_time_to_full_secs,
    }
}

#[cfg(target_os = "macos")]
fn memory_used_percent(system: &System) -> f64 {
    macos_memory_pressure_used().unwrap_or_else(|| sysinfo_used_percent(system))
}

#[cfg(not(target_os = "macos"))]
fn memory_used_percent(system: &System) -> f64 {
    sysinfo_used_percent(system)
}

fn sysinfo_used_percent(system: &System) -> f64 {
    let total = system.total_memory();
    if total == 0 {
        return 0.0;
    }
    let available = system.available_memory().min(total);
    100.0 * (total - available) as f64 / total as f64
}
/// Parse `memory_pressure`'s "System-wide memory free percentage: NN%" and return
/// used = 100 - free, the figure Activity Monitor's memory-pressure graph reflects.
///
/// Shared across extension hosts and held for [`MEMORY_PRESSURE_TTL`], since every host sampling
/// metrics on its own meant a subprocess per host per metrics tick.
#[cfg(target_os = "macos")]
fn macos_memory_pressure_used() -> Option<f64> {
    static CACHED: Mutex<Option<(Instant, f64)>> = Mutex::new(None);

    let mut cached = CACHED.lock().ok()?;
    if let Some((sampled_at, used)) = *cached
        && sampled_at.elapsed() < MEMORY_PRESSURE_TTL
    {
        return Some(used);
    }
    let used = macos_memory_pressure_sample()?;
    *cached = Some((Instant::now(), used));
    Some(used)
}

#[cfg(target_os = "macos")]
fn macos_memory_pressure_sample() -> Option<f64> {
    let output = std::process::Command::new("/usr/bin/memory_pressure")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let free: f64 = text
        .lines()
        .find_map(|line| line.split("free percentage:").nth(1))
        .and_then(|rest| rest.trim().trim_end_matches('%').trim().parse().ok())?;
    Some((100.0 - free).clamp(0.0, 100.0))
}

/// Charge percentage, AC state, and remaining battery time. A machine with no battery
/// (desktop, or a probe error) reports `(None, true, None, None)` so the bar shows an AC icon.
fn battery_status(
    manager: Option<&BatteryManager>,
) -> (Option<f32>, bool, Option<f32>, Option<f32>) {
    let Some(manager) = manager else {
        return (None, true, None, None);
    };
    let Ok(mut batteries) = manager.batteries() else {
        return (None, true, None, None);
    };
    match batteries.next() {
        Some(Ok(battery)) => {
            let percent = battery.state_of_charge().value * 100.0;
            let on_ac = matches!(battery.state(), BatteryState::Charging | BatteryState::Full);
            let time_to_empty = battery.time_to_empty().map(|time| time.get::<second>());
            let time_to_full = battery.time_to_full().map(|time| time.get::<second>());
            (Some(percent), on_ac, time_to_empty, time_to_full)
        }
        _ => (None, true, None, None),
    }
}

fn json_value_to_lua(lua: &Lua, value: serde_json::Value) -> mlua::Result<Value> {
    match value {
        serde_json::Value::Null => Ok(Value::Nil),
        serde_json::Value::Bool(value) => Ok(Value::Boolean(value)),
        serde_json::Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Ok(Value::Integer(value))
            } else if let Some(value) = value.as_u64() {
                if let Ok(value) = i64::try_from(value) {
                    Ok(Value::Integer(value))
                } else {
                    Ok(Value::Number(value as f64))
                }
            } else {
                Ok(Value::Number(value.as_f64().unwrap_or_default()))
            }
        }
        serde_json::Value::String(value) => Ok(Value::String(lua.create_string(&value)?)),
        serde_json::Value::Array(values) => {
            let table = lua.create_table_with_capacity(values.len(), 0)?;
            for (index, value) in values.into_iter().enumerate() {
                table.set(index + 1, json_value_to_lua(lua, value)?)?;
            }
            Ok(Value::Table(table))
        }
        serde_json::Value::Object(entries) => {
            let table = lua.create_table_with_capacity(0, entries.len())?;
            for (key, value) in entries {
                table.set(key, json_value_to_lua(lua, value)?)?;
            }
            Ok(Value::Table(table))
        }
    }
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
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| std::io::Error::other("bootty.exec needs a program to run"))?;
    let mut command = Command::new(program);
    command.args(args);
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
fn platform_shell_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

#[cfg(not(windows))]
fn platform_shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(windows)]
const fn windows_no_window_flag() -> u32 {
    0x0800_0000
}

#[derive(Clone)]
pub(crate) struct ExtensionFacts {
    theme: Arc<RwLock<Vec<(String, String)>>>,
    mux: Arc<RwLock<MuxView>>,
    metrics: Arc<RwLock<Metrics>>,
    session_reorders: Arc<RwLock<Vec<SessionReorder>>>,
    run_cache: Arc<RunCache>,
    processes: ManagedProcesses,
}

#[derive(Clone)]
pub(crate) struct ExtensionFactGeneration {
    pub(super) catalog: Arc<CommandCatalog>,
    pub(super) identity: ModuleIdentity,
    pub(super) generation: u64,
    pub(super) control: Arc<WorkerControl>,
}

fn install_process_interface(
    lua: &Lua,
    bootty: &Table,
    processes: ManagedProcesses,
    generation: Option<ExtensionFactGeneration>,
) -> mlua::Result<()> {
    let process = lua.create_table()?;

    let start_processes = processes.clone();
    let start_generation = generation.clone();
    process.set(
        "start",
        lua.create_function(move |lua, spec: Table| {
            let generation = require_process_mutation(start_generation.as_ref())?;
            let id = spec.get::<String>("id")?;
            let argv = spec
                .get::<Table>("argv")?
                .sequence_values::<String>()
                .collect::<mlua::Result<Vec<_>>>()?;
            let cwd = spec.get::<Option<String>>("cwd")?;
            let status = generation
                .catalog
                .with_active_extension_generation(
                    generation.identity.as_str(),
                    generation.generation,
                    || start_processes.start(id, argv, cwd.as_deref().map(std::path::Path::new)),
                )
                .map_err(mlua::Error::runtime)?
                .map_err(mlua::Error::runtime)?;
            process_status_table(lua, status)
        })?,
    )?;

    let write_processes = processes.clone();
    let write_generation = generation.clone();
    process.set(
        "write",
        lua.create_function(move |_, (id, line): (String, String)| {
            let generation = require_process_mutation(write_generation.as_ref())?;
            generation
                .catalog
                .with_active_extension_generation(
                    generation.identity.as_str(),
                    generation.generation,
                    || write_processes.write(&id, line),
                )
                .map_err(mlua::Error::runtime)?
                .map_err(mlua::Error::runtime)
        })?,
    )?;

    let poll_processes = processes.clone();
    let poll_generation = generation.clone();
    process.set(
        "poll",
        lua.create_function(move |lua, (id, limit): (String, Option<usize>)| {
            let generation = poll_generation.as_ref().ok_or_else(|| {
                mlua::Error::runtime("extension processes are unavailable during preview")
            })?;
            let events = generation
                .catalog
                .with_active_extension_generation(
                    generation.identity.as_str(),
                    generation.generation,
                    || poll_processes.poll(&id, limit.unwrap_or(64)),
                )
                .map_err(mlua::Error::runtime)?
                .map_err(mlua::Error::runtime)?;
            let output = lua.create_table_with_capacity(events.len(), 0)?;
            for (index, event) in events.into_iter().enumerate() {
                output.set(index + 1, process_event_table(lua, event)?)?;
            }
            Ok(output)
        })?,
    )?;

    let status_processes = processes.clone();
    let status_generation = generation.clone();
    process.set(
        "status",
        lua.create_function(move |lua, id: String| {
            let generation = status_generation.as_ref().ok_or_else(|| {
                mlua::Error::runtime("extension processes are unavailable during preview")
            })?;
            let status = generation
                .catalog
                .with_active_extension_generation(
                    generation.identity.as_str(),
                    generation.generation,
                    || status_processes.status(&id),
                )
                .map_err(mlua::Error::runtime)?
                .map_err(mlua::Error::runtime)?;
            process_status_table(lua, status)
        })?,
    )?;

    let stop_generation = generation;
    process.set(
        "stop",
        lua.create_function(move |_, id: String| {
            let generation = require_process_mutation(stop_generation.as_ref())?;
            generation
                .catalog
                .with_active_extension_generation(
                    generation.identity.as_str(),
                    generation.generation,
                    || processes.stop(&id),
                )
                .map_err(mlua::Error::runtime)?
                .map_err(mlua::Error::runtime)
        })?,
    )?;

    process.set_readonly(true);
    bootty.set("process", process)
}

fn require_process_mutation(
    generation: Option<&ExtensionFactGeneration>,
) -> mlua::Result<&ExtensionFactGeneration> {
    let generation = generation.ok_or_else(|| {
        mlua::Error::runtime("extension processes are unavailable during preview")
    })?;
    let active = generation
        .control
        .active
        .lock()
        .map_err(|_| mlua::Error::runtime("extension invocation lock poisoned"))?;
    if active.is_none() {
        return Err(mlua::Error::runtime(
            "bootty.process mutation is available only inside a command or action handler",
        ));
    }
    drop(active);
    Ok(generation)
}

fn process_status_table(lua: &Lua, status: ProcessStatus) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set("running", status.running)?;
    table.set("queued", status.queued)?;
    table.set("dropped", status.dropped)?;
    Ok(table)
}

fn process_event_table(lua: &Lua, event: ProcessEvent) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    match event {
        ProcessEvent::Stdout(line) => {
            table.set("stream", "stdout")?;
            table.set("line", line)?;
        }
        ProcessEvent::Stderr(line) => {
            table.set("stream", "stderr")?;
            table.set("line", line)?;
        }
        ProcessEvent::Error(message) => {
            table.set("stream", "error")?;
            table.set("line", message)?;
        }
        ProcessEvent::Exit(code) => {
            table.set("stream", "exit")?;
            table.set("code", code)?;
        }
        ProcessEvent::Dropped(count) => {
            table.set("stream", "dropped")?;
            table.set("count", count)?;
        }
    }
    Ok(table)
}

impl ExtensionFacts {
    pub(crate) fn new(theme: Vec<(String, String)>) -> Self {
        Self {
            theme: Arc::new(RwLock::new(theme)),
            mux: Arc::default(),
            metrics: Arc::default(),
            session_reorders: Arc::default(),
            run_cache: Arc::default(),
            processes: ManagedProcesses::default(),
        }
    }

    pub(crate) fn preview(theme: Vec<(String, String)>) -> Self {
        Self {
            theme: Arc::new(RwLock::new(theme)),
            mux: Arc::new(RwLock::new(preview_mux_view())),
            metrics: Arc::new(RwLock::new(Metrics {
                cpu: 42.0,
                load1: 1.25,
                mem_used_pct: 68.0,
                mem_total_bytes: 16 * 1_073_741_824,
                battery_percent: Some(73.0),
                on_ac: false,
                battery_time_to_empty_secs: Some(9_000.0),
                battery_time_to_full_secs: None,
            })),
            session_reorders: Arc::default(),
            run_cache: preview_run_cache(),
            processes: ManagedProcesses::default(),
        }
    }

    pub(crate) fn for_generation(&self) -> Self {
        Self {
            theme: Arc::clone(&self.theme),
            mux: Arc::clone(&self.mux),
            metrics: Arc::clone(&self.metrics),
            session_reorders: Arc::clone(&self.session_reorders),
            run_cache: Arc::default(),
            processes: ManagedProcesses::default(),
        }
    }

    pub(crate) fn retire(&self) {
        self.run_cache.retire();
        self.processes.retire();
    }

    pub(crate) fn install(
        &self,
        lua: &Lua,
        bootty: &Table,
        generation: Option<ExtensionFactGeneration>,
    ) -> mlua::Result<()> {
        install_ui_host_interface(lua, bootty, self, generation)
    }

    pub(crate) fn update_mux(&self, view: MuxView) -> bool {
        let Ok(mut current) = self.mux.write() else {
            return false;
        };
        if *current == view {
            return false;
        }
        *current = view;
        true
    }

    pub(crate) fn set_theme(&self, theme: Vec<(String, String)>) -> bool {
        let Ok(mut current) = self.theme.write() else {
            return false;
        };
        if *current == theme {
            return false;
        }
        *current = theme;
        true
    }

    pub(crate) fn metrics(&self) -> Metrics {
        self.metrics
            .read()
            .map(|metrics| *metrics)
            .unwrap_or_default()
    }

    pub(crate) fn update_metrics(&self, metrics: Metrics) -> bool {
        let Ok(mut current) = self.metrics.write() else {
            return false;
        };
        if *current == metrics {
            return false;
        }
        *current = metrics;
        true
    }

    pub(crate) fn take_session_reorders(&self) -> Vec<SessionReorder> {
        self.session_reorders
            .write()
            .map(|mut queue| std::mem::take(&mut *queue))
            .unwrap_or_default()
    }
}

fn install_ui_host_interface(
    lua: &Lua,
    bootty: &Table,
    facts: &ExtensionFacts,
    generation: Option<ExtensionFactGeneration>,
) -> mlua::Result<()> {
    let theme = facts
        .theme
        .read()
        .map_err(|_| mlua::Error::runtime("extension theme lock poisoned"))?
        .clone();
    let mux = Arc::clone(&facts.mux);
    let metrics = Arc::clone(&facts.metrics);
    let session_reorders = Arc::clone(&facts.session_reorders);
    let run_cache = Arc::clone(&facts.run_cache);
    install_process_interface(lua, bootty, facts.processes.clone(), generation.clone())?;
    // Shell out and return trimmed stdout, via the platform shell. Prefer
    // `bootty.metrics()` for system stats, which is native and cross-platform.
    // Render phases return cached output immediately and refresh in the background,
    // so a slow provider/command cannot block unrelated modules.
    let run_shell_cache = Arc::clone(&run_cache);
    bootty.set(
        "run",
        lua.create_function(move |_, cmd: String| {
            run_shell_cache.run(&cmd).map_err(mlua::Error::external)
        })?,
    )?;
    // Run a program directly from its argument vector: no shell process in front of it, no quoting
    // to get wrong. Use it for anything that needs no shell syntax; `bootty.run` covers the rest.
    let exec_cache = Arc::clone(&run_cache);
    bootty.set(
        "exec",
        lua.create_function(move |_, argv: Vec<String>| {
            exec_cache.exec(argv).map_err(mlua::Error::external)
        })?,
    )?;
    // Read what a command last printed without starting it. Pair with `bootty.exec` on a schedule:
    // exec keeps the answer current, read shows it as soon as it arrives and costs nothing.
    let read_cache = Arc::clone(&run_cache);
    bootty.set(
        "read",
        lua.create_function(move |_, argv: Vec<String>| Ok(read_cache.read(argv)))?,
    )?;
    let shell_table = lua.create_table()?;
    let shell_run_cache = Arc::clone(&run_cache);
    shell_table.set(
        "run",
        lua.create_function(move |_, cmd: String| {
            shell_run_cache.run(&cmd).map_err(mlua::Error::external)
        })?,
    )?;
    shell_table.set(
        "quote",
        lua.create_function(|_, value: String| Ok(platform_shell_quote(&value)))?,
    )?;
    shell_table.set(
        "stderr_null",
        if cfg!(windows) {
            "2>nul"
        } else {
            "2>/dev/null"
        },
    )?;
    shell_table.set_readonly(true);
    bootty.set("shell", shell_table)?;

    let path_table = lua.create_table()?;
    path_table.set(
        "display",
        lua.create_function(|_, value: String| Ok(crate::strings::display_path(&value)))?,
    )?;
    path_table.set_readonly(true);
    bootty.set("path", path_table)?;

    let git_table = lua.create_table()?;
    let git_preview_branch = run_cache.preview_branch.clone();
    git_table.set(
        "branch",
        lua.create_function(move |_, cwd: String| {
            Ok(match &git_preview_branch {
                Some(branch) => Some(branch.clone()),
                None => crate::git::head_branch(&cwd),
            })
        })?,
    )?;
    // A counter for the working tree, bumped by the filesystem whenever something under it changes.
    // A module compares it against the value from its last `git` call to know whether asking again
    // could possibly say anything new. `0` means the tree is not watched and nothing can be assumed.
    let git_watch_previews = run_cache.preview_branch.is_some();
    git_table.set(
        "worktree_revision",
        lua.create_function(move |_, cwd: String| {
            Ok(if git_watch_previews {
                0
            } else {
                crate::git::worktree_revision(&cwd)
            })
        })?,
    )?;
    git_table.set_readonly(true);
    bootty.set("git", git_table)?;

    bootty.set(
        "time",
        lua.create_function(|_, ()| {
            Ok(SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0.0, |duration| duration.as_secs_f64()))
        })?,
    )?;

    let json_table = lua.create_table()?;
    json_table.set(
        "decode",
        lua.create_function(|lua, text: String| {
            let value = serde_json::from_str(&text).map_err(mlua::Error::external)?;
            json_value_to_lua(lua, value)
        })?,
    )?;
    json_table.set(
        "encode",
        lua.create_function(|_, value: Value| {
            serde_json::to_string(&super::lua_value(value, 0)?).map_err(mlua::Error::external)
        })?,
    )?;
    json_table.set_readonly(true);
    bootty.set("json", json_table)?;

    // Mux state: the active session's windows, and the session name.
    let windows_mux = Arc::clone(&mux);
    bootty.set(
        "windows",
        lua.create_function(move |lua, ()| {
            let array = lua.create_table()?;
            if let Ok(view) = windows_mux.read() {
                for (index, window) in view.windows.iter().enumerate() {
                    let entry = lua.create_table()?;
                    entry.set("id", window.id.as_str())?;
                    entry.set("index", window.index)?;
                    entry.set("name", window.name.as_str())?;
                    entry.set("active", window.active)?;
                    entry.set("progress", window.progress)?;
                    entry.set("progress_indeterminate", window.progress_indeterminate)?;
                    array.set(index + 1, entry)?;
                }
            }
            Ok(array)
        })?,
    )?;
    let sessions_mux = Arc::clone(&mux);
    bootty.set(
        "sessions",
        lua.create_function(move |lua, ()| {
            let array = lua.create_table()?;
            if let Ok(view) = sessions_mux.read() {
                for (index, session) in view.sessions.iter().enumerate() {
                    let entry = lua.create_table()?;
                    entry.set("id", session.id.as_str())?;
                    entry.set("cache_key", format!("{}:{}", view.scope_key, session.id))?;
                    entry.set("name", session.name.as_str())?;
                    entry.set(
                        "display_name",
                        if session.display_name.is_empty() {
                            session.name.as_str()
                        } else {
                            session.display_name.as_str()
                        },
                    )?;
                    entry.set("active", session.active)?;
                    entry.set("selected", session.selected)?;
                    entry.set("progress", session.progress)?;
                    entry.set("progress_indeterminate", session.progress_indeterminate)?;
                    let progresses = lua.create_table()?;
                    for (progress_index, progress) in session.progresses.iter().enumerate() {
                        let progress_entry = lua.create_table()?;
                        progress_entry.set("process", progress.process.as_str())?;
                        progress_entry.set("value", progress.value)?;
                        progress_entry.set("indeterminate", progress.indeterminate)?;
                        progresses.set(progress_index + 1, progress_entry)?;
                    }
                    entry.set("progresses", progresses)?;
                    let ports = lua.create_table()?;
                    for (port_index, port) in session.ports.iter().enumerate() {
                        ports.set(port_index + 1, *port)?;
                    }
                    entry.set("ports", ports)?;
                    if let Some(value) = &session.cwd {
                        entry.set("cwd", value.as_str())?;
                    }
                    if let Some(value) = &session.pane_id {
                        entry.set("pane_id", value.as_str())?;
                    }
                    if let Some(value) = session.pane_pid {
                        entry.set("pane_pid", value)?;
                    }
                    if let Some(value) = &session.process {
                        entry.set("process", value.as_str())?;
                    }
                    if let Some(value) = &session.color {
                        entry.set("color", value.as_str())?;
                    }
                    if let Some(value) = &session.dim_color {
                        entry.set("dim_color", value.as_str())?;
                    }
                    array.set(index + 1, entry)?;
                }
            }
            Ok(array)
        })?,
    )?;
    let session_mux = Arc::clone(&mux);
    bootty.set(
        "session",
        lua.create_function(move |_, ()| {
            Ok(session_mux
                .read()
                .ok()
                .and_then(|view| view.session.clone()))
        })?,
    )?;
    let color_mux = Arc::clone(&mux);
    bootty.set(
        "session_color",
        lua.create_function(move |_, ()| {
            Ok(color_mux
                .read()
                .ok()
                .and_then(|view| view.session_color.clone()))
        })?,
    )?;
    let awake_mux = Arc::clone(&mux);
    bootty.set(
        "awake",
        lua.create_function(move |_, ()| {
            Ok(awake_mux
                .read()
                .map(|view| view.keep_awake)
                .unwrap_or(false))
        })?,
    )?;

    // Ask Bootty to apply a session-order change to its native session-order store. Modules
    // call this from `on_reorder` to reorder bootty-owned sessions; the app drains and applies
    // it on the main thread. `before` nil means "move to the end".
    bootty.set(
        "reorder_session",
        lua.create_function(move |_, (source, before): (String, Option<String>)| {
            let reorder = SessionReorder { source, before };
            if let Some(generation) = generation.as_ref() {
                generation
                    .catalog
                    .with_active_extension_generation(
                        generation.identity.as_str(),
                        generation.generation,
                        || {
                            session_reorders
                                .write()
                                .map_err(|_| {
                                    mlua::Error::runtime("extension session reorder lock poisoned")
                                })?
                                .push(reorder);
                            Ok::<(), mlua::Error>(())
                        },
                    )
                    .map_err(mlua::Error::runtime)??;
            } else if let Ok(mut queue) = session_reorders.write() {
                queue.push(reorder);
            }
            Ok(())
        })?,
    )?;

    // Native, cross-platform system metrics. `load1` is 0 where the OS has no load
    // average (e.g. Windows); fall back to `cpu` there. `mem_pct` is the used
    // percentage (real memory pressure on macOS); `mem_used`/`mem_total` are GiB
    // and stay consistent with `mem_pct`.
    bootty.set(
        "metrics",
        lua.create_function(move |lua, ()| {
            let m = metrics.read().map(|m| *m).unwrap_or_default();
            let table = lua.create_table()?;
            table.set("cpu", m.cpu)?;
            table.set("load1", m.load1)?;
            let total_gib = m.mem_total_bytes as f64 / 1_073_741_824.0;
            table.set("mem_total", total_gib)?;
            table.set("mem_pct", m.mem_used_pct)?;
            table.set("mem_used", total_gib * m.mem_used_pct / 100.0)?;
            if let Some(secs) = m.battery_time_to_empty_secs {
                table.set("battery_time_to_empty", secs)?;
            }
            if let Some(secs) = m.battery_time_to_full_secs {
                table.set("battery_time_to_full", secs)?;
            }
            // `battery` is nil on a machine with no battery; `on_ac` is true when
            // plugged in / charging / full (or no battery).
            if let Some(percent) = m.battery_percent {
                table.set("battery", percent)?;
            }
            table.set("on_ac", m.on_ac)?;
            Ok(table)
        })?,
    )?;

    let ui_table: Table = lua
        .load(EXTENSION_UI_PRELUDE)
        .set_name("bootty.ui")
        .eval()?;
    ui_table.set(
        "shell_quote",
        lua.create_function(|_, value: String| Ok(platform_shell_quote(&value)))?,
    )?;
    ui_table.set(
        "stderr_null",
        if cfg!(windows) {
            "2>nul"
        } else {
            "2>/dev/null"
        },
    )?;
    bootty.set("ui", ui_table)?;
    let sidebar_table: Table = lua
        .load(SIDEBAR_FACTS_PRELUDE)
        .set_name("bootty.sidebar")
        .eval()?;
    let sidebar_mux = Arc::clone(&mux);
    sidebar_table.set(
        "visible",
        lua.create_function(move |_, ()| {
            Ok(sidebar_mux
                .read()
                .map(|view| view.sidebar_visible)
                .unwrap_or(false))
        })?,
    )?;
    sidebar_table.set_readonly(true);
    bootty.set("sidebar", sidebar_table)?;

    // Palette tokens so modules style with theme colors: `fg = bootty.theme.accent`.
    let theme_table = lua.create_table()?;
    for (name, hex) in &theme {
        theme_table.set(name.as_str(), hex.as_str())?;
    }
    theme_table.set_readonly(true);
    bootty.set("theme", theme_table)?;
    Ok(())
}
