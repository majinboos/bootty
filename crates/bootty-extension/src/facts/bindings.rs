use std::sync::Arc;
use std::time::SystemTime;

use mlua::{Lua, LuaSerdeExt, Table, Value, serde::SerializeOptions};

use super::run::platform_shell_quote;
use super::{ExtensionFactGeneration, ExtensionFacts};
use crate::fact_values::SessionReorder;
use crate::facts::QueuedSessionReorder;
use crate::git_helpers::{display_path, head_branch, worktree_revision};
use crate::module_runtime::{lua_value, require_active};
use crate::processes::{
    ManagedProcesses, ProcessEvent, ProcessStatus, ProcessTree, descendant_processes,
};

const EXTENSION_UI_PRELUDE: &str = include_str!("../extension_ui.luau");
const SIDEBAR_FACTS_PRELUDE: &str = include_str!("../sidebar_session_facts.luau");
const STDERR_NULL: &str = if cfg!(windows) {
    "2>nul"
} else {
    "2>/dev/null"
};

fn json_value_to_lua(lua: &Lua, value: &serde_json::Value) -> mlua::Result<Value> {
    lua.to_value_with(
        value,
        SerializeOptions::new()
            .set_array_metatable(false)
            .serialize_none_to_null(false)
            .serialize_unit_to_null(false),
    )
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
                .with_active_generation(generation.identity.as_str(), generation.generation, || {
                    start_processes.start(id, &argv, cwd.as_deref().map(std::path::Path::new))
                })
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
                .with_active_generation(generation.identity.as_str(), generation.generation, || {
                    write_processes.write(&id, line)
                })
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
                .with_active_generation(generation.identity.as_str(), generation.generation, || {
                    poll_processes.poll(&id, limit.unwrap_or(64))
                })
                .map_err(mlua::Error::runtime)?
                .map_err(mlua::Error::runtime)?;
            let output = lua.create_table_with_capacity(events.len(), 0)?;
            for (index, event) in events.into_iter().enumerate() {
                output.set(index + 1, process_event_table(lua, event)?)?;
            }
            Ok(output)
        })?,
    )?;

    let stop_generation = generation;
    process.set(
        "stop",
        lua.create_function(move |_, id: String| {
            let generation = require_process_mutation(stop_generation.as_ref())?;
            generation
                .catalog
                .with_active_generation(generation.identity.as_str(), generation.generation, || {
                    processes.stop(&id)
                })
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
    require_active(
        &generation.control,
        "bootty.process mutation is available only inside a command or action handler",
    )?;
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
    let (stream, line) = match event {
        ProcessEvent::Stdout(line) => ("stdout", line),
        ProcessEvent::Stderr(line) => ("stderr", line),
        ProcessEvent::Error(line) => ("error", line),
        ProcessEvent::Exit(code) => {
            table.set("stream", "exit")?;
            table.set("code", code)?;
            return Ok(table);
        }
        ProcessEvent::Dropped(count) => {
            table.set("stream", "dropped")?;
            table.set("count", count)?;
            return Ok(table);
        }
    };
    table.set("stream", stream)?;
    table.set("line", line)?;
    Ok(table)
}

impl ExtensionFacts {
    pub(crate) fn install(
        &self,
        lua: &Lua,
        bootty: &Table,
        generation: Option<ExtensionFactGeneration>,
    ) -> mlua::Result<()> {
        install_ui_host_interface(lua, bootty, self, generation)
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
    let home = facts.home.clone();
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
    shell_table.set("stderr_null", STDERR_NULL)?;
    shell_table.set_readonly(true);
    bootty.set("shell", shell_table)?;

    // Walk a process subtree natively. Modules used to shell out to `ps -axo` and rebuild the
    // whole machine's tree in Lua, which cost a full process listing several times a second.
    let process_tree = std::sync::Mutex::new(ProcessTree::default());
    bootty.set(
        "descendants",
        lua.create_function(move |lua, root_pid: u32| {
            let table = lua.create_table()?;
            let Ok(mut tree) = process_tree.lock() else {
                return Ok(table);
            };
            for (index, descendant) in descendant_processes(&mut tree, root_pid)
                .into_iter()
                .enumerate()
            {
                let entry = lua.create_table()?;
                entry.set("command", descendant.command)?;
                entry.set("args", descendant.args)?;
                table.set(index + 1, entry)?;
            }
            Ok(table)
        })?,
    )?;

    let path_table = lua.create_table()?;
    path_table.set(
        "display",
        lua.create_function(move |_, value: String| Ok(display_path(&value, home.as_deref())))?,
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
                None => head_branch(&cwd),
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
                worktree_revision(&cwd)
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
            json_value_to_lua(lua, &value)
        })?,
    )?;
    json_table.set(
        "encode",
        lua.create_function(|_, value: Value| {
            serde_json::to_string(&lua_value(value, 0)?).map_err(mlua::Error::external)
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
    let sessions_source = Arc::clone(&mux);
    bootty.set(
        "sessions",
        lua.create_function(move |lua, ()| {
            let array = lua.create_table()?;
            if let Ok(view) = sessions_source.read() {
                for (index, session) in view.sessions.iter().enumerate() {
                    let entry = lua.create_table()?;
                    entry.set("id", session.id.as_str())?;
                    entry.set("cache_key", format!("{}:{}", view.scope_key, session.id))?;
                    entry.set("name", session.name.as_str())?;
                    let display_name = if session.display_name.is_empty() {
                        session.name.as_str()
                    } else {
                        session.display_name.as_str()
                    };
                    entry.set("display_name", display_name)?;
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
    let current_session_source = Arc::clone(&mux);
    bootty.set(
        "session",
        lua.create_function(move |_, ()| {
            Ok(current_session_source
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
        lua.create_function(move |_, ()| Ok(awake_mux.read().is_ok_and(|view| view.keep_awake)))?,
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
                    .with_active_generation(
                        generation.identity.as_str(),
                        generation.generation,
                        || {
                            session_reorders
                                .write()
                                .map_err(|_| {
                                    mlua::Error::runtime("extension session reorder lock poisoned")
                                })?
                                .push(QueuedSessionReorder {
                                    reorder,
                                    generation: Some((
                                        generation.identity.clone(),
                                        generation.generation,
                                        generation.control.generation.clone(),
                                    )),
                                });
                            Ok::<(), mlua::Error>(())
                        },
                    )
                    .map_err(mlua::Error::runtime)??;
            } else if let Ok(mut queue) = session_reorders.write() {
                queue.push(QueuedSessionReorder {
                    reorder,
                    generation: None,
                });
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
            let metrics = metrics.read().map(|metrics| *metrics).unwrap_or_default();
            let table = lua.create_table()?;
            table.set("cpu", metrics.cpu)?;
            table.set("load1", metrics.load1)?;
            let total_gib = metrics.mem_total_bytes as f64 / 1_073_741_824.0;
            table.set("mem_total", total_gib)?;
            table.set("mem_pct", metrics.mem_used_pct)?;
            table.set("mem_used", total_gib * metrics.mem_used_pct / 100.0)?;
            if let Some(secs) = metrics.battery_time_to_empty_secs {
                table.set("battery_time_to_empty", secs)?;
            }
            if let Some(secs) = metrics.battery_time_to_full_secs {
                table.set("battery_time_to_full", secs)?;
            }
            // `battery` is nil on a machine with no battery; `on_ac` is true when
            // plugged in / charging / full (or no battery).
            if let Some(percent) = metrics.battery_percent {
                table.set("battery", percent)?;
            }
            table.set("on_ac", metrics.on_ac)?;
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
    ui_table.set("stderr_null", STDERR_NULL)?;
    bootty.set("ui", ui_table)?;
    let sidebar_table: Table = lua
        .load(SIDEBAR_FACTS_PRELUDE)
        .set_name("bootty.sidebar")
        .eval()?;
    let sidebar_mux = Arc::clone(&mux);
    sidebar_table.set(
        "visible",
        lua.create_function(move |_, ()| {
            Ok(sidebar_mux.read().is_ok_and(|view| view.sidebar_visible))
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
