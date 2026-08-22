//! Login-shell environment hydration.
//!
//! A macOS `.app` launched from Finder/Dock/Spotlight inherits launchd's minimal
//! environment: PATH is `/usr/bin:/bin:/usr/sbin:/sbin` and the user's shell
//! exports (Homebrew, rustup, custom PATH entries) are absent. That breaks the
//! tmux backend and any tool the user spawns, since both rely on PATH. Launching
//! the same binary from a terminal works only because the terminal hands us its
//! environment. We reproduce that here by running the login shell once and
//! importing what it exports.

use bootty_runtime::terminal_session::{BOOTTY_SHELL_ENV, configured_user_shell};
#[cfg(target_os = "macos")]
static LOGIN_SHELL_ENVIRONMENT: std::sync::OnceLock<Option<Vec<(String, String)>>> =
    std::sync::OnceLock::new();

/// Align `$SHELL` with the OS account login shell before bootty spawns anything.
///
/// tmux captures `$SHELL` as a new server's `default-shell`, so a stale value
/// (zsh inherited from a dev launch or an old launchd session) makes every pane
/// spawn the wrong shell even when the account login shell is fish. The OS
/// account record is the source of truth; an explicit `BOOTTY_SHELL` wins first.
pub fn align_shell_env() {
    let Some(shell) = advertised_shell(
        std::env::var(BOOTTY_SHELL_ENV).ok(),
        configured_user_shell(),
    ) else {
        return;
    };
    if std::env::var("SHELL").ok().as_deref() == Some(shell.as_str()) {
        return;
    }
    // SAFETY: runs at startup before any threads are spawned.
    unsafe { std::env::set_var("SHELL", shell) };
}

/// The value `$SHELL` should advertise: an explicit override, then the login
/// shell, taking the first that is an absolute path. `None` leaves `$SHELL` as
/// inherited (e.g. non-macOS, where no account shell is resolved).
pub fn advertised_shell(
    override_shell: Option<String>,
    login_shell: Option<String>,
) -> Option<String> {
    [override_shell, login_shell]
        .into_iter()
        .flatten()
        .find(|shell| std::path::Path::new(shell).is_absolute())
}

/// Import the login shell's environment before Bootty spawns child processes.
///
/// Terminal launches often already carry a useful environment, but that is not a reliable proxy
/// for the account login shell's environment. Always importing the login shell PATH keeps
/// `bootty.run`, mux backends, and terminal sessions on the same command lookup surface.
#[cfg(target_os = "macos")]
pub fn hydrate_from_login_shell() {
    let Some(vars) = login_shell_environment() else {
        return;
    };

    apply_env(vars);
}

#[cfg(target_os = "macos")]
fn login_shell_environment() -> Option<Vec<(String, String)>> {
    LOGIN_SHELL_ENVIRONMENT
        .get_or_init(|| {
            let shell = login_shell();
            capture_login_env(&shell)
        })
        .clone()
}

#[cfg(not(target_os = "macos"))]
pub fn hydrate_from_login_shell() {}

#[cfg(target_os = "macos")]
fn login_shell() -> String {
    selected_login_shell(
        bootty_runtime::terminal_session::configured_user_shell(),
        std::env::var("SHELL").ok(),
    )
}

#[cfg(target_os = "macos")]
pub fn selected_login_shell(configured: Option<String>, inherited: Option<String>) -> String {
    [configured, inherited]
        .into_iter()
        .flatten()
        .find(|shell| std::path::Path::new(shell).is_absolute())
        .unwrap_or_else(|| "/bin/sh".to_string())
}

/// Run `<shell> -l -c env` and parse its output. A login shell sources the
/// profile files where PATH is set (e.g. `.zprofile` with `brew shellenv`), so
/// `-l` is enough to recover the user's PATH without the noise of an interactive
/// shell. Null-delimited output keeps multi-line values intact.
#[cfg(target_os = "macos")]
fn capture_login_env(shell: &str) -> Option<Vec<(String, String)>> {
    use std::process::Command;

    // `env -0` (null-delimited) is unambiguous even when a value contains
    // newlines; `printenv` lacks the option, but `env` from coreutils/BSD on
    // macOS supports `-0`.
    let output = Command::new(shell)
        .args(["-l", "-c", "/usr/bin/env -0"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    Some(parse_login_environment(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

#[cfg(target_os = "macos")]
pub fn parse_login_environment(raw: &str) -> Vec<(String, String)> {
    raw.split('\0')
        .filter_map(|entry| {
            let (key, value) = entry.split_once('=')?;
            if key.is_empty() {
                return None;
            }
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

/// Adopt the login shell's PATH unconditionally (the value we actually came for)
/// and fill in any other exports the current process is missing. Variables
/// launchd already set (HOME, USER, TMPDIR, the security session) are left
/// alone so we don't clobber the platform's own values.
#[cfg(target_os = "macos")]
fn apply_env(vars: Vec<(String, String)>) {
    for (key, value) in vars {
        if should_apply_login_environment(&key, std::env::var_os(&key).is_some()) {
            // SAFETY: hydration runs at startup before any threads are spawned.
            unsafe {
                std::env::set_var(&key, &value);
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub fn should_apply_login_environment(key: &str, current_present: bool) -> bool {
    key == "PATH" || !current_present
}
