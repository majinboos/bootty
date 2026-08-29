mod command_runtime;

use anyhow::{Result, bail};

fn main() -> Result<()> {
    let (identity, args) =
        command_runtime::parse_application_identity(std::env::args().skip(1).collect())?;
    bootty_rmux::prepare_local_rmux_daemon(identity)?;
    if let Some(code) = bootty_rmux::run_embedded_rmux_daemon()? {
        std::process::exit(code);
    }
    match args.first().map(String::as_str) {
        Some("remote-ping") => {
            command_runtime::run_remote_ping();
            Ok(())
        }
        Some("remote-exec") => command_runtime::run_remote_exec(&args[1..]),
        Some("remote-rmux") => command_runtime::run_remote_rmux(&args[1..]),
        Some("remote-space") => {
            let paths = command_runtime::remote_space_paths_from_environment(identity)?;
            command_runtime::run_remote_space(&args[1..], &paths)
        }
        Some("remote-project") => command_runtime::run_remote_project(&args[1..]),
        Some("remote-worktree") => command_runtime::run_remote_worktree(&args[1..]),
        Some(command) => bail!("unknown command {command:?}"),
        None => bail!("bootty-daemon requires a command"),
    }
}
