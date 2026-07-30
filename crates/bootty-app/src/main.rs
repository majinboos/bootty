#![cfg_attr(windows, windows_subsystem = "windows")]

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    if let Some(code) = bootty_mux::run_embedded_rmux_daemon()? {
        std::process::exit(code);
    }
    // Correct a stale `$SHELL` to the OS login shell before any child inherits
    // it; tmux otherwise bakes the wrong shell into the server's default-shell.
    bootty_app::shell_env::align_shell_env();
    // Recover the user's PATH and shell exports before anything reads the
    // environment; a Finder-launched .app starts with launchd's minimal PATH.
    bootty_app::shell_env::hydrate_from_login_shell();

    let cli = bootty_app::cli::Cli::parse();
    let window_state_key = cli.window_state_key().to_owned();
    let config = cli.load_config()?;
    let options = bootty_app::platform::native_options_for_config(&config);

    bootty_app::native_host::run(options, config, window_state_key)
}
