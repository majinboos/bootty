#![cfg_attr(windows, windows_subsystem = "windows")]

use std::{process::ExitCode, sync::Arc};

use anyhow::Result;
use bootty_app::{application_identity::ApplicationIdentity, remote_catalog};
use bootty_cli::{Cli, Command, EventCommand, RemoteSpaceCommand, TaskCommand, UpdateResult};
use bootty_command::{Caller, CommandInvocation};
use bootty_control as control;
use clap::Parser;

mod cli_runtime;
#[cfg(target_os = "macos")]
mod macos_cli;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::from(cli_runtime::exit_code(&error))
        }
    }
}

fn run() -> Result<()> {
    let identity = ApplicationIdentity::current();
    bootty_rmux::prepare_local_rmux_daemon(identity)?;
    if let Some(code) = bootty_rmux::run_embedded_rmux_daemon()? {
        std::process::exit(code);
    }
    bootty_native::link();
    bootty_rmux::link();
    bootty_tmux::link();
    bootty_zellij::link();
    let backends = Arc::new(bootty_mux::provider::MuxBackendRegistry::desktop()?);
    // Correct a stale `$SHELL` to the OS login shell before any child inherits
    // it; tmux otherwise bakes the wrong shell into the server's default-shell.
    bootty_cli::align_shell_env();
    // Recover the user's PATH and shell exports before anything reads the
    // environment; a Finder-launched .app starts with launchd's minimal PATH.
    bootty_cli::hydrate_from_login_shell();
    #[cfg(target_os = "macos")]
    if let Err(error) = macos_cli::ensure_cli_link() {
        eprintln!("Could not install the Bootty command: {error}");
    }

    let cli = Cli::parse();
    match cli.subcommand() {
        Some(Command::Commands) => {
            cli_runtime::print_control_response(
                cli_runtime::control_request(cli.start(), "command.list", serde_json::Value::Null)?,
                cli.json(),
            )?;
            return Ok(());
        }
        Some(Command::Describe { name }) => {
            cli_runtime::print_control_response(
                cli_runtime::control_request(
                    cli.start(),
                    "command.describe",
                    serde_json::json!({"command": name}),
                )?,
                cli.json(),
            )?;
            return Ok(());
        }
        Some(Command::Invoke {
            name,
            arguments,
            yes,
            detached,
        }) => {
            let invocation = CommandInvocation::new(name, arguments.clone(), Caller::Cli);
            let response = cli_runtime::invoke_control_command(&cli, invocation, *yes, *detached)?;
            if *detached {
                cli_runtime::print_control_response(response, cli.json())?;
            } else {
                cli_runtime::print_command_response(response, cli.json())?;
            }
            return Ok(());
        }
        Some(Command::Task(command)) => {
            let (method, params) = match command {
                TaskCommand::Status { task } => ("task.status", serde_json::json!({"task": task})),
                TaskCommand::Cancel { task } => ("task.cancel", serde_json::json!({"task": task})),
            };
            cli_runtime::print_control_response(
                cli_runtime::control_request(cli.start(), method, params)?,
                cli.json(),
            )?;
            return Ok(());
        }
        Some(Command::Events(command)) => {
            let (method, params) = match command {
                EventCommand::Subscribe { topics } => {
                    ("event.subscribe", serde_json::json!({"topics": topics}))
                }
                EventCommand::Poll {
                    subscription,
                    cursor,
                } => (
                    "event.subscribe",
                    serde_json::json!({
                        "subscription": subscription,
                        "cursor": cursor,
                    }),
                ),
                EventCommand::Unsubscribe { subscription } => (
                    "event.unsubscribe",
                    serde_json::json!({"subscription": subscription}),
                ),
            };
            cli_runtime::print_control_response(
                cli_runtime::control_request(cli.start(), method, params)?,
                cli.json(),
            )?;
            return Ok(());
        }
        Some(Command::Dynamic(arguments)) => {
            cli_runtime::invoke_dynamic_command(&cli, arguments)?;
            return Ok(());
        }
        Some(Command::Update) => {
            match bootty_cli::update(true)? {
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
                            &backends,
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
                    remote_catalog::execute(&config, &backends, id, (*backend).into(), payload)?;
                }
            }
            return Ok(());
        }
        Some(Command::RemoteExec { payload }) => {
            std::process::exit(bootty_remote::run_remote_command(payload)?);
        }
        Some(Command::RemotePing) => return Ok(()),
        Some(Command::RemoteRmux { payload }) => {
            std::process::exit(bootty_rmux::run_remote_rmux_command(payload)?);
        }
        None => {}
    }
    if control::running_instance()?.is_some() {
        return Ok(());
    }
    if matches!(bootty_cli::automatic_update(), Ok(UpdateResult::Updated)) {
        bootty_cli::restart_after_update()?;
        return Ok(());
    }

    let window_state_key = cli.window_state_key().to_owned();
    let config = cli.load_config()?;
    let options = bootty_app::platform::native_options_for_config(&config);

    bootty_app::native_host::run(options, config, window_state_key, backends)
}
