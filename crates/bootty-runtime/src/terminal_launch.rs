use std::{env, path::Path, thread};

#[cfg(target_os = "macos")]
use std::process::Command as ProcessCommand;

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::terminal_session::SessionLaunchConfig;
use bootty_terminal::terminal_engine::{TERMINAL_PROGRAM, TERMINAL_PROGRAM_VERSION};

pub const BOOTTY_SHELL_ENV: &str = "BOOTTY_SHELL";

const TERM_ENV: &str = "TERM";
const COLORTERM_ENV: &str = "COLORTERM";
const TERMINFO_ENV: &str = "TERMINFO";
const TERM_PROGRAM_ENV: &str = "TERM_PROGRAM";
const TERM_PROGRAM_VERSION_ENV: &str = "TERM_PROGRAM_VERSION";
const BOOTTY_PANE_ENV: &str = "BOOTTY_PANE";

#[cfg(windows)]
const DEFAULT_SHELL: &str = "powershell.exe";
#[cfg(not(windows))]
const DEFAULT_SHELL: &str = "/bin/sh";

pub(crate) struct SpawnedTerminal {
    master: Box<dyn MasterPty + Send>,
    child: OwnedChild,
    tty_name: Option<String>,
}

impl SpawnedTerminal {
    pub(crate) fn into_parts(self) -> (Box<dyn MasterPty + Send>, OwnedChild, Option<String>) {
        (self.master, self.child, self.tty_name)
    }
}

pub(crate) struct OwnedChild(Option<Box<dyn Child + Send + Sync>>);

impl OwnedChild {
    fn new(child: Box<dyn Child + Send + Sync>) -> Self {
        Self(Some(child))
    }

    pub(crate) fn exited(&mut self) -> Result<bool> {
        self.0
            .as_mut()
            .expect("owned child must exist")
            .try_wait()
            .map(|status| status.is_some())
            .context("poll shell child process")
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let Some(mut child) = self.0.take() else {
            return;
        };
        let _ = child.kill();
        thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

pub(crate) fn spawn(size: PtySize, config: &SessionLaunchConfig) -> Result<SpawnedTerminal> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(size)?;

    let shell = shell_command_path(config.shell.clone());
    let launch_env = resolve_launch_environment(config, crate::terminfo::vendored_terminfo_dir());
    let mut command = CommandBuilder::new(shell);
    command.args(&config.args);
    for (name, value) in locale_env_entries() {
        command.env(name, value);
    }
    for (name, value) in &launch_env.env {
        command.env(name, value);
    }
    for name in &config.env_remove {
        command.env_remove(name);
    }
    command.env(TERM_ENV, &launch_env.term);
    command.env(COLORTERM_ENV, &config.colorterm);
    command.env(
        TERM_PROGRAM_ENV,
        config.term_program.as_deref().unwrap_or(TERMINAL_PROGRAM),
    );
    command.env(TERM_PROGRAM_VERSION_ENV, TERMINAL_PROGRAM_VERSION);
    // Pane identity for programs that report which visible terminal they run in. Only backends that
    // spawn the pane's own PTY know it; a tmux attach leaves it unset because tmux exports the same
    // id as `$TMUX_PANE` inside each of its panes.
    if let Some(pane_id) = &config.pane_id {
        command.env(BOOTTY_PANE_ENV, pane_id);
    }
    if let Some(terminfo) = &launch_env.terminfo {
        command.env(TERMINFO_ENV, terminfo.to_string_lossy().into_owned());
    }
    if let Some(cwd) = &config.working_directory {
        command.cwd(cwd);
    }

    #[cfg(unix)]
    let tty_name = pair
        .master
        .tty_name()
        .map(|path| path.to_string_lossy().into_owned());
    #[cfg(not(unix))]
    let tty_name: Option<String> = None;

    let child = pair
        .slave
        .spawn_command(command)
        .context("spawn shell in PTY")?;

    Ok(SpawnedTerminal {
        master: pair.master,
        child: OwnedChild::new(child),
        tty_name,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedLaunchEnvironment {
    term: String,
    terminfo: Option<std::path::PathBuf>,
    env: Vec<(String, String)>,
}

fn resolve_launch_environment(
    config: &SessionLaunchConfig,
    bootty_terminfo_dir: Option<&Path>,
) -> ResolvedLaunchEnvironment {
    let (term, terminfo) = if config.term == crate::terminfo::XTERM_BOOTTY {
        match bootty_terminfo_dir {
            Some(dir) => (config.term.clone(), Some(dir.to_path_buf())),
            None => ("xterm-256color".to_owned(), None),
        }
    } else {
        (config.term.clone(), None)
    };
    let env = config
        .env
        .iter()
        .filter(|(name, _)| !is_managed_launch_env(name))
        .cloned()
        .collect();

    ResolvedLaunchEnvironment {
        term,
        terminfo,
        env,
    }
}

fn is_managed_launch_env(name: &str) -> bool {
    matches!(
        name,
        TERM_ENV
            | COLORTERM_ENV
            | TERMINFO_ENV
            | TERM_PROGRAM_ENV
            | TERM_PROGRAM_VERSION_ENV
            | BOOTTY_PANE_ENV
    )
}

pub fn configured_user_shell() -> Option<String> {
    configured_login_shell()
}

fn locale_env_entries() -> Vec<(String, String)> {
    let mut entries = Vec::new();
    for key in ["LANG", "LC_ALL", "LC_CTYPE"] {
        if let Ok(value) = env::var(key) {
            entries.push((key.to_owned(), value));
        }
    }
    for (key, value) in env::vars() {
        if key.starts_with("LC_") && !entries.iter().any(|(existing, _)| existing == &key) {
            entries.push((key, value));
        }
    }
    if !entries.iter().any(|(key, _)| key == "LC_CTYPE")
        && let Some((_, lang)) = entries.iter().find(|(key, _)| key == "LANG")
    {
        entries.push(("LC_CTYPE".to_owned(), lang.clone()));
    }
    normalize_locale_entries(&mut entries);
    entries
}

#[cfg(target_os = "macos")]
fn normalize_locale_entries(entries: &mut Vec<(String, String)>) {
    for (_, value) in entries.iter_mut() {
        if is_macos_c_locale(value) {
            *value = "en_US.UTF-8".to_owned();
        }
    }
    for missing in ["LANG", "LC_CTYPE"] {
        if !entries.iter().any(|(key, _)| key == missing) {
            entries.push((missing.to_owned(), "en_US.UTF-8".to_owned()));
        }
    }
}

#[cfg(target_os = "macos")]
fn is_macos_c_locale(value: &str) -> bool {
    matches!(value, "C" | "POSIX" | "C.UTF-8" | "C.utf8")
}

#[cfg(not(target_os = "macos"))]
fn normalize_locale_entries(_entries: &mut Vec<(String, String)>) {}

fn shell_command_path(configured: Option<String>) -> String {
    [
        env::var(BOOTTY_SHELL_ENV).ok(),
        configured,
        configured_user_shell(),
        env::var("SHELL").ok(),
    ]
    .into_iter()
    .flatten()
    .find_map(normalize_shell_path)
    .unwrap_or_else(|| DEFAULT_SHELL.to_string())
}

fn normalize_shell_path(shell: String) -> Option<String> {
    let shell = shell.trim();
    if shell.is_empty() || !Path::new(shell).is_absolute() {
        return None;
    }
    Some(shell.to_string())
}

#[cfg(target_os = "macos")]
fn configured_login_shell() -> Option<String> {
    [
        env::var("USER").ok(),
        env::var("LOGNAME").ok(),
        current_username(),
    ]
    .into_iter()
    .flatten()
    .find_map(normalize_username)
    .and_then(|user| read_login_shell_for_user(&user))
}

#[cfg(target_os = "macos")]
fn normalize_username(user: String) -> Option<String> {
    let user = user.trim();
    if user.is_empty() || user.contains('/') {
        return None;
    }
    Some(user.to_string())
}

#[cfg(target_os = "macos")]
fn current_username() -> Option<String> {
    let output = ProcessCommand::new("/usr/bin/id")
        .arg("-un")
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    normalize_username(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(target_os = "macos")]
fn read_login_shell_for_user(user: &str) -> Option<String> {
    let user_record = format!("/Users/{user}");
    let output = ProcessCommand::new("/usr/bin/dscl")
        .args([".", "-read", user_record.as_str(), "UserShell"])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    parse_user_shell_output(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(target_os = "macos"))]
fn configured_login_shell() -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
fn parse_user_shell_output(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let (_, shell) = line.split_once(':')?;
        normalize_shell_path(shell.to_string())
    })
}
