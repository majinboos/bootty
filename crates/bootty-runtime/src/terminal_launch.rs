use std::{env, path::Path, thread};

#[cfg(target_os = "macos")]
use std::process::Command as ProcessCommand;

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::terminal_session::SessionLaunchConfig;
use bootty_surface::geometry::TerminalGeometry;
use bootty_terminal::terminal_engine::{TERMINAL_PROGRAM, TERMINAL_PROGRAM_VERSION};

pub const BOOTTY_SHELL_ENV: &str = "BOOTTY_SHELL";

const TERM_ENV: &str = "TERM";
const COLORTERM_ENV: &str = "COLORTERM";
const TERMINFO_ENV: &str = "TERMINFO";
const TERM_PROGRAM_ENV: &str = "TERM_PROGRAM";
const TERM_PROGRAM_VERSION_ENV: &str = "TERM_PROGRAM_VERSION";

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

pub(crate) fn spawn(
    geometry: TerminalGeometry,
    config: &SessionLaunchConfig,
) -> Result<SpawnedTerminal> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows: geometry.rows,
        cols: geometry.cols,
        pixel_width: geometry.pixel_width(),
        pixel_height: geometry.pixel_height(),
    })?;

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
    command.env(COLORTERM_ENV, &launch_env.colorterm);
    command.env(TERM_PROGRAM_ENV, &launch_env.term_program);
    command.env(TERM_PROGRAM_VERSION_ENV, &launch_env.term_program_version);
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
    colorterm: String,
    terminfo: Option<std::path::PathBuf>,
    term_program: String,
    term_program_version: String,
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
        colorterm: config.colorterm.clone(),
        terminfo,
        term_program: TERMINAL_PROGRAM.to_owned(),
        term_program_version: TERMINAL_PROGRAM_VERSION.to_owned(),
        env,
    }
}

fn is_managed_launch_env(name: &str) -> bool {
    matches!(
        name,
        TERM_ENV | COLORTERM_ENV | TERMINFO_ENV | TERM_PROGRAM_ENV | TERM_PROGRAM_VERSION_ENV
    )
}

pub fn configured_user_shell() -> Option<String> {
    configured_login_shell()
}

fn locale_env_entries() -> Vec<(String, String)> {
    let mut entries = Vec::new();
    for key in ["LANG", "LC_ALL", "LC_CTYPE"] {
        push_env_if_present(&mut entries, key);
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

fn push_env_if_present(entries: &mut Vec<(String, String)>, key: &str) {
    if let Ok(value) = env::var(key) {
        entries.push((key.to_owned(), value));
    }
}

#[cfg(target_os = "macos")]
fn normalize_locale_entries(entries: &mut Vec<(String, String)>) {
    for (_, value) in entries.iter_mut() {
        if is_macos_c_locale(value) {
            *value = "en_US.UTF-8".to_owned();
        }
    }
    if !entries.iter().any(|(key, _)| key == "LANG") {
        entries.push(("LANG".to_owned(), "en_US.UTF-8".to_owned()));
    }
    if !entries.iter().any(|(key, _)| key == "LC_CTYPE") {
        entries.push(("LC_CTYPE".to_owned(), "en_US.UTF-8".to_owned()));
    }
}

#[cfg(target_os = "macos")]
fn is_macos_c_locale(value: &str) -> bool {
    matches!(value, "C" | "POSIX" | "C.UTF-8" | "C.utf8")
}

#[cfg(not(target_os = "macos"))]
fn normalize_locale_entries(_entries: &mut Vec<(String, String)>) {}

fn shell_command_path(configured: Option<String>) -> String {
    select_shell_path(
        env::var(BOOTTY_SHELL_ENV).ok(),
        configured,
        configured_user_shell(),
        env::var("SHELL").ok(),
    )
}

fn select_shell_path(
    explicit: Option<String>,
    configured: Option<String>,
    login: Option<String>,
    inherited: Option<String>,
) -> String {
    [explicit, configured, login, inherited]
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
    configured_login_shell_with(
        env::var("USER").ok(),
        env::var("LOGNAME").ok(),
        current_username(),
        read_login_shell_for_user,
    )
}

#[cfg(target_os = "macos")]
fn configured_login_shell_with(
    user: Option<String>,
    logname: Option<String>,
    current: Option<String>,
    mut read_shell: impl FnMut(&str) -> Option<String>,
) -> Option<String> {
    let user = select_configured_shell_username(user, logname, current)?;
    read_shell(&user)
}

#[cfg(target_os = "macos")]
fn select_configured_shell_username(
    user: Option<String>,
    logname: Option<String>,
    current: Option<String>,
) -> Option<String> {
    [user, logname, current]
        .into_iter()
        .flatten()
        .find_map(normalize_username)
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
        .ok()?;
    if !output.status.success() {
        return None;
    }
    normalize_username(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(target_os = "macos")]
fn read_login_shell_for_user(user: &str) -> Option<String> {
    let user_record = format!("/Users/{user}");
    let output = ProcessCommand::new("/usr/bin/dscl")
        .args([".", "-read", user_record.as_str(), "UserShell"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
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
