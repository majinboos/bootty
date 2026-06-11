use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

use anyhow::{Context, Result};

const XTERM_GHOSTTY_TERMINFO_SRC: &str = include_str!("../assets/xterm-ghostty.terminfo");
pub const XTERM_GHOSTTY: &str = "xterm-ghostty";

/// The vendored xterm-ghostty terminfo database, compiled on demand into
/// Bootty's state directory. Sessions resolve it through the TERMINFO
/// environment variable, mirroring how Ghostty ships its own entry inside
/// the app bundle instead of installing into the system database.
pub fn vendored_terminfo_dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let state_dir = bootty_state_dir()?;
        ensure_xterm_ghostty_terminfo_in(&state_dir).ok()
    })
    .as_deref()
}

pub fn terminfo_env_entry(term: &str, env: &[(String, String)]) -> Option<(String, String)> {
    if term != XTERM_GHOSTTY {
        return None;
    }
    if env.iter().any(|(name, _)| name == "TERMINFO") {
        return None;
    }
    let dir = vendored_terminfo_dir()?;
    Some(("TERMINFO".to_owned(), dir.to_string_lossy().into_owned()))
}

pub fn ensure_xterm_ghostty_terminfo_in(state_dir: &Path) -> Result<PathBuf> {
    let db_dir = state_dir.join("terminfo");
    if compiled_entry_exists(&db_dir) {
        return Ok(db_dir);
    }

    fs::create_dir_all(state_dir)
        .with_context(|| format!("create bootty state dir {}", state_dir.display()))?;
    let source_path = state_dir.join("xterm-ghostty.terminfo");
    fs::write(&source_path, XTERM_GHOSTTY_TERMINFO_SRC)
        .with_context(|| format!("write terminfo source {}", source_path.display()))?;

    let output = Command::new("tic")
        .arg("-x")
        .arg("-o")
        .arg(&db_dir)
        .arg(&source_path)
        .output()
        .context("run tic to compile xterm-ghostty terminfo")?;
    anyhow::ensure!(
        output.status.success(),
        "tic failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    anyhow::ensure!(
        compiled_entry_exists(&db_dir),
        "tic reported success but produced no xterm-ghostty entry in {}",
        db_dir.display()
    );
    Ok(db_dir)
}

fn compiled_entry_exists(db_dir: &Path) -> bool {
    // ncurses stores entries under a first-letter dir on Linux and a hex
    // dir ("78" for 'x') on macOS.
    db_dir.join("78").join(XTERM_GHOSTTY).is_file()
        || db_dir.join("x").join(XTERM_GHOSTTY).is_file()
}

fn bootty_state_dir() -> Option<PathBuf> {
    if let Some(xdg_state) = env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(xdg_state).join("bootty"));
    }
    let home = env::var_os("HOME").filter(|value| !value.is_empty())?;
    Some(PathBuf::from(home).join(".local/state/bootty"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendored_terminfo_compiles_and_resolves_via_terminfo_env() -> Result<()> {
        let state = tempfile::tempdir()?;
        let db_dir = ensure_xterm_ghostty_terminfo_in(state.path())?;

        let resolved = Command::new("infocmp")
            .env("TERMINFO", &db_dir)
            .arg(XTERM_GHOSTTY)
            .output()?;
        assert!(
            resolved.status.success(),
            "infocmp could not resolve xterm-ghostty: {}",
            String::from_utf8_lossy(&resolved.stderr)
        );
        Ok(())
    }

    #[test]
    fn ensure_reuses_existing_compiled_entry() -> Result<()> {
        let state = tempfile::tempdir()?;
        let db_dir = ensure_xterm_ghostty_terminfo_in(state.path())?;
        let entry = ["78", "x"]
            .iter()
            .map(|prefix| db_dir.join(prefix).join(XTERM_GHOSTTY))
            .find(|path| path.is_file())
            .expect("compiled entry");
        let compiled_at = entry.metadata()?.modified()?;

        let again = ensure_xterm_ghostty_terminfo_in(state.path())?;

        assert_eq!(again, db_dir);
        assert_eq!(entry.metadata()?.modified()?, compiled_at);
        Ok(())
    }

    #[test]
    fn terminfo_env_entry_respects_user_override_and_foreign_terms() {
        let user_env = vec![("TERMINFO".to_owned(), "/custom".to_owned())];
        assert_eq!(terminfo_env_entry(XTERM_GHOSTTY, &user_env), None);
        assert_eq!(terminfo_env_entry("xterm-256color", &[]), None);
    }
}
