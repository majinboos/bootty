use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

use anyhow::{Context, Result};

const XTERM_BOOTTY_TERMINFO_SRC: &str = include_str!("../assets/xterm-bootty.terminfo");
pub const XTERM_BOOTTY: &str = "xterm-bootty";

/// The vendored xterm-bootty terminfo database, compiled on demand into
/// Bootty's state directory. Sessions resolve it through the TERMINFO
/// environment variable.
pub fn vendored_terminfo_dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let state_dir = bootty_state_dir()?;
        ensure_xterm_bootty_terminfo_in(&state_dir).ok()
    })
    .as_deref()
}

pub fn ensure_xterm_bootty_terminfo_in(state_dir: &Path) -> Result<PathBuf> {
    let db_dir = state_dir.join("terminfo");
    let source_path = state_dir.join("xterm-bootty.terminfo");
    if compiled_entry_exists(&db_dir) && vendored_source_current(&source_path) {
        return Ok(db_dir);
    }

    fs::create_dir_all(state_dir)
        .with_context(|| format!("create bootty state dir {}", state_dir.display()))?;
    fs::write(&source_path, XTERM_BOOTTY_TERMINFO_SRC)
        .with_context(|| format!("write terminfo source {}", source_path.display()))?;

    let output = Command::new("tic")
        .arg("-x")
        .arg("-o")
        .arg(&db_dir)
        .arg(&source_path)
        .output()
        .context("run tic to compile xterm-bootty terminfo")?;
    anyhow::ensure!(
        output.status.success(),
        "tic failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    anyhow::ensure!(
        compiled_entry_exists(&db_dir),
        "tic reported success but produced no xterm-bootty entry in {}",
        db_dir.display()
    );
    Ok(db_dir)
}

fn compiled_entry_exists(db_dir: &Path) -> bool {
    // ncurses stores entries under a first-letter dir on Linux and a hex
    // dir ("78" for 'x') on macOS.
    db_dir.join("78").join(XTERM_BOOTTY).is_file() || db_dir.join("x").join(XTERM_BOOTTY).is_file()
}

fn vendored_source_current(source_path: &Path) -> bool {
    fs::read_to_string(source_path).is_ok_and(|source| source == XTERM_BOOTTY_TERMINFO_SRC)
}
fn bootty_state_dir() -> Option<PathBuf> {
    if let Some(xdg_state) = env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(xdg_state).join("bootty"));
    }
    let home = env::var_os("HOME").filter(|value| !value.is_empty())?;
    Some(PathBuf::from(home).join(".local/state/bootty"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn vendored_terminfo_compiles_and_resolves_via_terminfo_env() -> Result<()> {
        let state = tempfile::tempdir()?;
        let db_dir = ensure_xterm_bootty_terminfo_in(state.path())?;

        let resolved = Command::new("infocmp")
            .env("TERMINFO", &db_dir)
            .arg(XTERM_BOOTTY)
            .output()?;
        assert!(
            resolved.status.success(),
            "infocmp could not resolve xterm-bootty: {}",
            String::from_utf8_lossy(&resolved.stderr)
        );
        Ok(())
    }

    #[test]
    fn vendored_terminfo_uses_bootty_identity_without_ghostty_aliases() -> Result<()> {
        let state = tempfile::tempdir()?;
        let db_dir = ensure_xterm_bootty_terminfo_in(state.path())?;

        let resolved = Command::new("infocmp")
            .env("TERMINFO", &db_dir)
            .arg(XTERM_BOOTTY)
            .output()?;
        assert!(
            resolved.status.success(),
            "infocmp could not resolve xterm-bootty: {}",
            String::from_utf8_lossy(&resolved.stderr)
        );
        let entry = String::from_utf8_lossy(&resolved.stdout);

        assert!(entry.contains("xterm-bootty|bootty|Bootty"));
        assert!(!entry.contains("xterm-ghostty"));
        assert!(!entry.contains("|ghostty|"));
        assert!(!entry.contains("|Ghostty"));
        Ok(())
    }

    #[test]
    fn vendored_terminfo_advertises_synchronized_output_caps() -> Result<()> {
        let state = tempfile::tempdir()?;
        let db_dir = ensure_xterm_bootty_terminfo_in(state.path())?;

        let resolved = Command::new("infocmp")
            .arg("-x")
            .env("TERMINFO", &db_dir)
            .arg(XTERM_BOOTTY)
            .output()?;
        assert!(
            resolved.status.success(),
            "infocmp could not resolve xterm-bootty: {}",
            String::from_utf8_lossy(&resolved.stderr)
        );
        let entry = String::from_utf8_lossy(&resolved.stdout);

        assert!(entry.contains("BSU=\\E[?2026h"));
        assert!(entry.contains("ESU=\\E[?2026l"));
        assert!(entry.contains("Sync=\\E[?2026"));
        Ok(())
    }

    #[test]
    fn vendored_terminfo_advertises_tmux_progress_bar_cap() -> Result<()> {
        let state = tempfile::tempdir()?;
        let db_dir = ensure_xterm_bootty_terminfo_in(state.path())?;

        let resolved = Command::new("infocmp")
            .arg("-x")
            .env("TERMINFO", &db_dir)
            .arg(XTERM_BOOTTY)
            .output()?;
        assert!(
            resolved.status.success(),
            "infocmp could not resolve xterm-bootty: {}",
            String::from_utf8_lossy(&resolved.stderr)
        );
        let entry = String::from_utf8_lossy(&resolved.stdout);

        // tmux forwards OSC 9;4 progress only when Spb resolves, and the
        // trailing ST is easy to mangle when escaping the terminfo source.
        assert!(
            entry.contains(r"Spb=\E]9;4;%p1%d;%p2%d\E\\"),
            "missing Spb: {entry}"
        );
        Ok(())
    }

    #[test]
    fn vendored_terminfo_advertises_only_supported_function_keys() -> Result<()> {
        let state = tempfile::tempdir()?;
        let db_dir = ensure_xterm_bootty_terminfo_in(state.path())?;

        let resolved = Command::new("infocmp")
            .arg("-x")
            .env("TERMINFO", &db_dir)
            .arg(XTERM_BOOTTY)
            .output()?;
        assert!(
            resolved.status.success(),
            "infocmp could not resolve xterm-bootty: {}",
            String::from_utf8_lossy(&resolved.stderr)
        );
        let entry = String::from_utf8_lossy(&resolved.stdout);

        for key in 1..=12 {
            assert!(entry.contains(&format!("kf{key}=")), "missing kf{key}");
        }
        for key in 13..=63 {
            assert!(
                !entry.contains(&format!("kf{key}=")),
                "unsupported kf{key} is advertised"
            );
        }
        Ok(())
    }

    #[test]
    fn ensure_reuses_existing_compiled_entry() -> Result<()> {
        let state = tempfile::tempdir()?;
        let db_dir = ensure_xterm_bootty_terminfo_in(state.path())?;
        let entry = ["78", "x"]
            .iter()
            .map(|prefix| db_dir.join(prefix).join(XTERM_BOOTTY))
            .find(|path| path.is_file())
            .expect("compiled entry");
        let compiled_at = entry.metadata()?.modified()?;

        let again = ensure_xterm_bootty_terminfo_in(state.path())?;

        assert_eq!(again, db_dir);
        assert_eq!(entry.metadata()?.modified()?, compiled_at);
        Ok(())
    }

    #[test]
    fn ensure_rewrites_stale_vendored_terminfo_source() -> Result<()> {
        let state = tempfile::tempdir()?;
        let db_dir = ensure_xterm_bootty_terminfo_in(state.path())?;
        let source_path = state.path().join("xterm-bootty.terminfo");
        fs::write(&source_path, "xterm-bootty|stale,\n\tam,\n")?;

        let again = ensure_xterm_bootty_terminfo_in(state.path())?;

        assert_eq!(again, db_dir);
        assert_eq!(fs::read_to_string(source_path)?, XTERM_BOOTTY_TERMINFO_SRC);
        Ok(())
    }
}
