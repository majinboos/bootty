use std::{path::PathBuf, process::Command};

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

pub const REMOTE_DAEMON_PROGRAM: &str = "bootty-daemon";
pub const REMOTE_DAEMON_PROTOCOL_VERSION: &str = "4";
#[cfg(feature = "app")]
pub const REMOTE_EXEC_PROGRAM: &str = concat!(
    "./.bootty/bin/bootty-daemon-",
    "4",
    "-",
    env!("CARGO_PKG_VERSION"),
    ".exe"
);
#[cfg(feature = "app")]
pub const REMOTE_EXEC_SUBCOMMAND: &str = "remote-exec";
#[cfg(feature = "app")]
pub const REMOTE_PING_SUBCOMMAND: &str = "remote-ping";
const MAX_REMOTE_COMMAND_PAYLOAD: usize = 1024 * 1024;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct RemoteCommand {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) terminal: bool,
}

#[cfg(feature = "app")]
pub(crate) fn proxy_command_line(program: &str, args: &[String], terminal: bool) -> Result<String> {
    let payload = serde_json::to_vec(&RemoteCommand {
        program: program.to_owned(),
        args: args.to_vec(),
        terminal,
    })
    .context("encode remote command")?;
    Ok(format!(
        "{REMOTE_EXEC_PROGRAM} {REMOTE_EXEC_SUBCOMMAND} {}",
        URL_SAFE_NO_PAD.encode(payload)
    ))
}

pub(crate) fn decode_remote_command(payload: &str) -> Result<RemoteCommand> {
    if payload.len() > MAX_REMOTE_COMMAND_PAYLOAD {
        bail!("remote command payload is too large")
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .context("decode remote command payload")?;
    let command: RemoteCommand =
        serde_json::from_slice(&bytes).context("parse remote command payload")?;
    if command.program.is_empty() {
        bail!("remote command program cannot be empty")
    }
    Ok(command)
}

pub fn run_remote_command(payload: &str) -> Result<i32> {
    let command = decode_remote_command(payload)?;
    let program = if command.program == REMOTE_DAEMON_PROGRAM {
        std::env::current_exe().context("resolve Bootty daemon executable")?
    } else {
        PathBuf::from(&command.program)
    };
    let mut child = Command::new(program);
    child.args(&command.args);
    if command.terminal {
        #[cfg(feature = "app")]
        if let Some(terminfo) = bootty_runtime::terminfo::vendored_terminfo_dir() {
            child.env("TERM", "xterm-bootty").env("TERMINFO", terminfo);
        } else {
            child.env("TERM", "xterm-256color");
        }
        #[cfg(not(feature = "app"))]
        child.env("TERM", "xterm-256color");
    }
    let status = child
        .status()
        .with_context(|| format!("run remote command {}", command.program))?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(all(feature = "app", test))]
pub(crate) fn decode_proxy_command_line(line: &str) -> Result<RemoteCommand> {
    let mut tokens = line.split_whitespace();
    if tokens.next() != Some(REMOTE_EXEC_PROGRAM) || tokens.next() != Some(REMOTE_EXEC_SUBCOMMAND) {
        bail!("not a Bootty remote proxy command")
    }
    let payload = tokens
        .next()
        .context("remote proxy command has no payload")?;
    if tokens.next().is_some() {
        bail!("remote proxy command has extra tokens")
    }
    decode_remote_command(payload)
}
