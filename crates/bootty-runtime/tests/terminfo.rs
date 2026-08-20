#![cfg(unix)]

use std::process::Command;

use anyhow::Result;
use bootty_runtime::terminfo::{XTERM_BOOTTY, ensure_xterm_bootty_terminfo_in};

fn compiled_entry(extra: bool) -> Result<String> {
    let state = tempfile::tempdir()?;
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
fn vendored_entry_advertises_supported_extended_capabilities() -> Result<()> {
    let entry = compiled_entry(true)?;

    assert!(entry.contains("BSU=\\E[?2026h"));
    assert!(entry.contains("ESU=\\E[?2026l"));
    assert!(entry.contains("Sync=\\E[?2026"));
    assert!(entry.contains(r"Spb=\E]9;4;%p1%d;%p2%d\E\\"));
    Ok(())
}

#[test]
fn vendored_entry_advertises_only_supported_function_keys() -> Result<()> {
    let entry = compiled_entry(true)?;

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
