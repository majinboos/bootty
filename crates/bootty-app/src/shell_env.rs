//! Login-shell environment hydration.
//!
//! A macOS `.app` launched from Finder/Dock/Spotlight inherits launchd's minimal
//! environment: PATH is `/usr/bin:/bin:/usr/sbin:/sbin` and the user's shell
//! exports (Homebrew, rustup, custom PATH entries) are absent. That breaks the
//! tmux backend and any tool the user spawns, since both rely on PATH. Launching
//! the same binary from a terminal works only because the terminal hands us its
//! environment. We reproduce that here by running the login shell once and
//! importing what it exports.

#[cfg(target_os = "macos")]
use std::io::IsTerminal;

/// Import the login shell's environment when the process was launched outside a
/// terminal (the GUI-launch case). No-op when already attached to a tty, so
/// terminal launches keep the environment the user already has.
#[cfg(target_os = "macos")]
pub fn hydrate_from_login_shell() {
    // A real terminal launch already carries the user's environment; only the
    // GUI launch path (no controlling tty) needs hydration.
    if std::io::stdin().is_terminal() {
        return;
    }

    let shell = login_shell();
    let Some(vars) = capture_login_env(&shell) else {
        return;
    };

    apply_env(vars);
}

#[cfg(not(target_os = "macos"))]
pub fn hydrate_from_login_shell() {}

#[cfg(target_os = "macos")]
fn login_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| std::path::Path::new(s).is_absolute())
        .unwrap_or_else(|| "/bin/zsh".to_string())
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

    Some(parse_null_delimited_env(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

#[cfg(target_os = "macos")]
fn parse_null_delimited_env(raw: &str) -> Vec<(String, String)> {
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
        if key == "PATH" || std::env::var_os(&key).is_none() {
            // SAFETY: hydration runs at startup before any threads are spawned.
            unsafe {
                std::env::set_var(&key, &value);
            }
        }
    }
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use super::parse_null_delimited_env;

    #[test]
    fn parses_entries_and_preserves_multiline_values() {
        let raw = "PATH=/opt/homebrew/bin:/usr/bin\0MULTI=line1\nline2\0EMPTY=\0";
        let parsed = parse_null_delimited_env(raw);

        assert_eq!(parsed.len(), 3);
        assert_eq!(
            parsed[0],
            ("PATH".to_string(), "/opt/homebrew/bin:/usr/bin".to_string())
        );
        assert_eq!(parsed[1], ("MULTI".to_string(), "line1\nline2".to_string()));
        assert_eq!(parsed[2], ("EMPTY".to_string(), String::new()));
    }

    #[test]
    fn skips_entries_without_a_key() {
        // A leading `=value` or stray separator must not produce an empty key.
        let parsed = parse_null_delimited_env("=orphan\0\0VALID=1\0");

        assert_eq!(parsed, vec![("VALID".to_string(), "1".to_string())]);
    }
}
