#![cfg_attr(windows, windows_subsystem = "windows")]

use anyhow::Result;
use bootty_app::{
    cli::{Cli, Command, RemoteSpaceCommand},
    remote_catalog,
    update::{self, UpdateResult},
};
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
    #[cfg(target_os = "macos")]
    if let Err(error) = ensure_macos_cli_link() {
        eprintln!("Could not install the Bootty command: {error}");
    }

    let cli = Cli::parse();
    match cli.subcommand() {
        Some(Command::Update) => {
            match update::update(true)? {
                UpdateResult::Updated => println!("Bootty was updated. Restart Bootty to use it."),
                UpdateResult::UpToDate => println!("Bootty is already up to date."),
                UpdateResult::Skipped => {}
            }
            return Ok(());
        }
        Some(Command::RemoteSpace(command)) => {
            let config = cli.load_config()?;
            match command {
                RemoteSpaceCommand::List => {
                    println!(
                        "{}",
                        serde_json::to_string(&remote_catalog::list(&config)?)?
                    );
                }
                RemoteSpaceCommand::Create { name, backend } => {
                    println!(
                        "{}",
                        serde_json::to_string(&remote_catalog::create(
                            &config,
                            name,
                            (*backend).into(),
                        )?)?
                    );
                }
                RemoteSpaceCommand::Snapshot { id, backend } => {
                    println!(
                        "{}",
                        serde_json::to_string(&remote_catalog::snapshot(
                            &config,
                            id,
                            (*backend).into(),
                        )?)?
                    );
                }
                RemoteSpaceCommand::Execute {
                    id,
                    backend,
                    payload,
                } => {
                    remote_catalog::execute(&config, id, (*backend).into(), payload)?;
                }
            }
            return Ok(());
        }
        Some(Command::RemoteExec { payload }) => {
            std::process::exit(bootty_mux::ssh::run_remote_command(payload)?);
        }
        Some(Command::RemotePing) => return Ok(()),
        Some(Command::RemoteRmux { payload }) => {
            std::process::exit(bootty_mux::run_remote_rmux_command(payload)?);
        }
        None => {}
    }
    if matches!(update::automatic_update(), Ok(UpdateResult::Updated)) {
        update::restart_after_update()?;
        return Ok(());
    }

    let window_state_key = cli.window_state_key().to_owned();
    let config = cli.load_config()?;
    let options = bootty_app::platform::native_options_for_config(&config);

    bootty_app::native_host::run(options, config, window_state_key)
}

#[cfg(target_os = "macos")]
fn ensure_macos_cli_link() -> std::io::Result<()> {
    let executable = std::env::current_exe()?;
    if !executable.ends_with("Contents/MacOS/bootty") {
        return Ok(());
    }
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return Ok(());
    };
    if let Some(path) = std::env::var_os("PATH") {
        let local_bin = home.join(".local/bin");
        let home_bin = home.join("bin");
        for directory in std::env::split_paths(&path).filter(|directory| {
            directory == &local_bin
                || directory == &home_bin
                || matches!(
                    directory.to_str(),
                    Some("/usr/local/bin" | "/opt/homebrew/bin")
                )
        }) {
            match install_macos_cli_link_at(&executable, &directory) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => continue,
                Err(error) => return Err(error),
            }
        }
    }
    let directory = home.join(".local/bin");
    install_macos_cli_link_at(&executable, &directory)?;
    ensure_macos_local_bin_path(&home, std::env::var_os("SHELL").as_deref())
}

#[cfg(target_os = "macos")]
fn ensure_macos_local_bin_path(
    home: &std::path::Path,
    shell: Option<&std::ffi::OsStr>,
) -> std::io::Result<()> {
    let shell_name = shell.and_then(|shell| std::path::Path::new(shell).file_name());
    let fish = shell_name.is_some_and(|name| name == "fish");
    let (profile, line) = if fish {
        (
            home.join(".config/fish/config.fish"),
            r#"fish_add_path "$HOME/.local/bin""#,
        )
    } else {
        (
            home.join(if shell_name.is_some_and(|name| name == "zsh") {
                ".zprofile"
            } else {
                ".profile"
            }),
            r#"export PATH="$HOME/.local/bin:$PATH""#,
        )
    };
    if let Some(parent) = profile.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents = std::fs::read_to_string(&profile).unwrap_or_default();
    if !contents.lines().any(|existing| existing == line) {
        use std::io::Write as _;
        writeln!(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(profile)?,
            "{line}"
        )?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn install_macos_cli_link_at(
    executable: &std::path::Path,
    directory: &std::path::Path,
) -> std::io::Result<()> {
    let link = directory.join("bootty");
    std::fs::create_dir_all(directory)?;
    match std::fs::symlink_metadata(&link) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            if std::fs::read_link(&link)? == executable {
                return Ok(());
            }
            std::fs::remove_file(&link)?;
        }
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::os::unix::fs::symlink(executable, link)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn macos_app_installs_its_cli_link() {
        let home = tempfile::tempdir().expect("home");
        let executable = home
            .path()
            .join("Applications/Bootty.app/Contents/MacOS/bootty");

        install_macos_cli_link_at(&executable, &home.path().join(".local/bin"))
            .expect("install CLI link");

        assert_eq!(
            std::fs::read_link(home.path().join(".local/bin/bootty")).expect("CLI link"),
            executable
        );
    }

    #[test]
    fn macos_app_adds_its_fallback_cli_directory_to_zsh_path() {
        let home = tempfile::tempdir().expect("home");

        ensure_macos_local_bin_path(home.path(), Some(std::ffi::OsStr::new("/bin/zsh")))
            .expect("configure PATH");

        assert_eq!(
            std::fs::read_to_string(home.path().join(".zprofile")).expect("profile"),
            "export PATH=\"$HOME/.local/bin:$PATH\"\n"
        );
    }
}
