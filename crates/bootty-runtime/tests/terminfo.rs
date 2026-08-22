#![cfg(unix)]

use std::process::Command;

use anyhow::Result;
use assert_fs::TempDir;
use bootty_runtime::terminfo::{XTERM_BOOTTY, ensure_xterm_bootty_terminfo_in};

fn compiled_entry(extra: bool) -> Result<String> {
    let state = TempDir::new()?;
    let database = ensure_xterm_bootty_terminfo_in(state.path())?;
    let mut command = Command::new("infocmp");
    if extra {
        command.arg("-x");
    }
    let output = command
        .env("TERMINFO", database)
        .arg(XTERM_BOOTTY)
        .output()?;
    assert!(
        output.status.success(),
        "infocmp failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

#[test]
fn vendored_entry_resolves_with_bootty_identity() -> Result<()> {
    let entry = compiled_entry(false)?;

    assert!(entry.contains("xterm-bootty|bootty|Bootty"));
    assert!(!entry.contains("ghostty"));
    Ok(())
}

#[test]
fn vendored_extended_entry_matches_supported_capabilities() -> Result<()> {
    let entry = compiled_entry(true)?;
    let missing = [
        "BSU=\\E[?2026h",
        "ESU=\\E[?2026l",
        "Sync=\\E[?2026",
        r"Spb=\E]9;4;%p1%d;%p2%d\E\\",
    ]
    .into_iter()
    .filter(|capability| !entry.contains(capability))
    .collect::<Vec<_>>();
    pretty_assertions::assert_eq!(missing, Vec::<&str>::new(), "compiled entry:\n{entry}");

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
