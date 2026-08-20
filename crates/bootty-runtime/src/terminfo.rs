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
