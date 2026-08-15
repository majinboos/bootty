use std::{env, ffi::OsStr, path::Path, sync::Arc};

use anyhow::Result;
use bootty_runtime::{TerminalSession, TerminalSessionConfig};

use super::pane::{PaneStartRequest, TerminalRuntime};

pub struct AttachLaunch {
    pub program: String,
    pub args: Vec<String>,
    pub env_remove: Vec<String>,
    pub env: Vec<(String, String)>,
    pub remote: bool,
}

pub fn start_attach_terminal(
    request: PaneStartRequest<'_>,
    launch: AttachLaunch,
) -> Result<Box<dyn TerminalRuntime>> {
    let mut config = request.terminal_config.clone();
    config.side_effect_pane_id = request.target.side_effect_pane_id();
    let config = attach_session_config(
        config,
        launch,
        bootty_runtime::terminfo::vendored_terminfo_dir().is_some(),
        env::var_os("PATH").as_deref(),
    )?;
    Ok(Box::new(TerminalSession::new_with_config(
        request.geometry,
        config,
        Arc::clone(request.repaint_wakeup),
    )?))
}

fn attach_session_config(
    mut config: TerminalSessionConfig,
    launch: AttachLaunch,
    bootty_terminfo_available: bool,
    path: Option<&OsStr>,
) -> Result<TerminalSessionConfig> {
    config.launch.shell = Some(resolve_launch_program_with_path(&launch.program, path)?);
    config.launch.args = launch.args;
    config.launch.env_remove = launch.env_remove;
    config.launch.env.extend(launch.env);

    let terminfo_reaches_client = bootty_terminfo_available && !launch.remote;
    if config.launch.term != bootty_runtime::terminfo::XTERM_BOOTTY || !terminfo_reaches_client {
        config.launch.term = "xterm-256color".to_owned();
    }
    Ok(config)
}

pub fn resolve_launch_program(program: &str) -> Result<String> {
    resolve_launch_program_with_path(program, env::var_os("PATH").as_deref())
}

pub fn resolve_launch_program_with_path(program: &str, path: Option<&OsStr>) -> Result<String> {
    if Path::new(program).is_absolute() {
        return Ok(program.to_owned());
    }
    if let Some(found) = path
        .into_iter()
        .flat_map(env::split_paths)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
    {
        return Ok(found.to_string_lossy().into_owned());
    }
    anyhow::bail!("backend attach program {program:?} not found in PATH")
}
