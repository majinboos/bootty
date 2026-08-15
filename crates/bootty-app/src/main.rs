#![cfg_attr(windows, windows_subsystem = "windows")]

use std::{fmt, io::Read, process::ExitCode};

use anyhow::{Context, Result};
#[cfg(target_os = "macos")]
use bootty_app::application_identity::ApplicationIdentity;
use bootty_app::{
    cli::{Cli, Command, RemoteSpaceCommand},
    commands::{Caller, CommandDescriptor, CommandInvocation, CommandTarget, ValueType},
    control, remote_catalog,
    update::{self, UpdateResult},
};
use clap::Parser;

const EXIT_USAGE: u8 = 2;
const EXIT_TRANSPORT: u8 = 4;
const EXIT_CONFIRMATION: u8 = 5;
const EXIT_DENIED: u8 = 6;
const EXIT_UNAVAILABLE: u8 = 7;
const EXIT_STALE_TARGET: u8 = 8;
const EXIT_COMMAND_FAILED: u8 = 9;

#[derive(Debug)]
struct CliFailure {
    code: u8,
    message: String,
}

impl fmt::Display for CliFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CliFailure {}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::from(
                error
                    .downcast_ref::<CliFailure>()
                    .map_or(1, |failure| failure.code),
            )
        }
    }
}

fn run() -> Result<()> {
    let identity = ApplicationIdentity::current();
    bootty_mux::prepare_local_rmux_daemon(identity)?;
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
        Some(Command::Commands) => {
            print_control_response(
                control_request(cli.start(), "command.list", serde_json::Value::Null)?,
                cli.json(),
            )?;
            return Ok(());
        }
        Some(Command::Describe { name }) => {
            print_control_response(
                control_request(
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
        }) => {
            let mut invocation = CommandInvocation::from_action(name, Caller::Cli);
            invocation.arguments = arguments.clone();
            print_command_response(invoke_control_command(&cli, invocation, *yes)?, cli.json())?;
            return Ok(());
        }
        Some(Command::Dynamic(arguments)) => {
            invoke_dynamic_command(&cli, arguments)?;
            return Ok(());
        }
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

fn invoke_dynamic_command(cli: &Cli, raw: &[String]) -> Result<()> {
    let (path, raw_arguments) = raw.split_first().context("missing command path")?;
    let name = path
        .split('.')
        .map(|segment| segment.replace('-', "_"))
        .collect::<Vec<_>>()
        .join(".");
    let instance = control::select_or_start(cli.start()).map_err(transport_failure)?;
    let mut described = control_instance_request(
        &instance,
        "command.describe",
        serde_json::json!({"command": name}),
    )?;
    if described
        .error
        .as_ref()
        .is_some_and(|error| error.code == -32602)
        && name.contains('.')
    {
        let leaf = name.rsplit('.').next().expect("dotted command has leaf");
        described = control_instance_request(
            &instance,
            "command.describe",
            serde_json::json!({"command": leaf}),
        )?;
    }
    if let Some(error) = described.error {
        return Err(rpc_failure(error));
    }
    let descriptor: CommandDescriptor = serde_json::from_value(
        described
            .result
            .context("command descriptor response contained no result")?,
    )?;
    if raw_arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        print_dynamic_help(path, &descriptor);
        return Ok(());
    }
    let (arguments, target, confirmed) = parse_dynamic_arguments(&descriptor, raw_arguments)?;
    let invocation = CommandInvocation {
        command: descriptor.id,
        arguments,
        caller: Caller::Cli,
        target,
        confirmation: None,
    };
    print_command_response(
        invoke_control_command_on_instance(&instance, invocation, confirmed)?,
        cli.json(),
    )
}

fn invoke_control_command(
    cli: &Cli,
    invocation: CommandInvocation,
    confirm: bool,
) -> Result<control::RpcResponse> {
    let descriptor = control::select_or_start(cli.start()).map_err(transport_failure)?;
    invoke_control_command_on_instance(&descriptor, invocation, confirm)
}

fn invoke_control_command_on_instance(
    descriptor: &control::InstanceDescriptor,
    mut invocation: CommandInvocation,
    confirm: bool,
) -> Result<control::RpcResponse> {
    let response = control_instance_request(
        descriptor,
        "command.invoke",
        serde_json::json!({"invocation": invocation}),
    )?;
    if !confirm {
        return Ok(response);
    }
    let Some(result) = response.result.as_ref() else {
        return Ok(response);
    };
    let outcome = serde_json::from_value::<bootty_app::commands::CommandOutcome>(result.clone())?;
    let bootty_app::commands::CommandOutcome::ConfirmationRequired { confirmation } = outcome
    else {
        return Ok(response);
    };
    invocation.confirmation = Some(*confirmation);
    control_instance_request(
        descriptor,
        "command.invoke",
        serde_json::json!({"invocation": invocation}),
    )
}

fn control_instance_request(
    descriptor: &control::InstanceDescriptor,
    method: &str,
    params: serde_json::Value,
) -> Result<control::RpcResponse> {
    control::invoke_instance(descriptor, method, params).map_err(transport_failure)
}

fn control_request(
    start: bool,
    method: &str,
    params: serde_json::Value,
) -> Result<control::RpcResponse> {
    control::invoke_or_start(start, method, params).map_err(transport_failure)
}

fn transport_failure(error: anyhow::Error) -> anyhow::Error {
    CliFailure {
        code: EXIT_TRANSPORT,
        message: error.to_string(),
    }
    .into()
}

fn rpc_failure(error: control::RpcError) -> anyhow::Error {
    let code = match error.code {
        -32700 | -32602..=-32600 => EXIT_USAGE,
        -32006 => EXIT_DENIED,
        _ => EXIT_TRANSPORT,
    };
    CliFailure {
        code,
        message: format!("{} ({})", error.message, error.code),
    }
    .into()
}

fn parse_dynamic_arguments(
    descriptor: &CommandDescriptor,
    raw: &[String],
) -> Result<(Vec<String>, Option<CommandTarget>, bool)> {
    let mut arguments = Vec::new();
    let mut target = None;
    let mut confirmed = false;
    let mut input = raw.iter();
    while let Some(argument) = input.next() {
        match argument.as_str() {
            "--yes" => confirmed = true,
            "--target" => {
                let value = input
                    .next()
                    .context("--target requires HANDLE@GENERATION")?;
                let (handle, generation) = value
                    .rsplit_once('@')
                    .context("--target requires HANDLE@GENERATION")?;
                target = Some(CommandTarget {
                    kind: descriptor
                        .target
                        .context("command does not accept a target")?,
                    handle: handle.to_owned(),
                    generation: generation.parse().context("invalid target generation")?,
                });
            }
            "--stdin-json" => {
                let mut json = String::new();
                std::io::stdin().read_to_string(&mut json)?;
                let values: Vec<serde_json::Value> = serde_json::from_str(&json)?;
                for value in values {
                    arguments.push(json_argument(value)?);
                }
            }
            option if option.starts_with("--") => {
                let expected = descriptor
                    .arguments
                    .arguments
                    .get(arguments.len())
                    .context("too many command arguments")?;
                let name = option.trim_start_matches("--");
                if expected.name.replace('_', "-") != name {
                    anyhow::bail!(
                        "expected --{}, got {option}",
                        expected.name.replace('_', "-")
                    );
                }
                arguments.push(
                    input
                        .next()
                        .with_context(|| format!("{option} requires a value"))?
                        .clone(),
                );
            }
            value => arguments.push(value.to_owned()),
        }
    }
    if arguments.len() != descriptor.arguments.arguments.len() {
        anyhow::bail!(
            "command {} expects {} argument(s), got {}",
            descriptor.id,
            descriptor.arguments.arguments.len(),
            arguments.len()
        );
    }
    Ok((arguments, target, confirmed))
}

fn json_argument(value: serde_json::Value) -> Result<String> {
    match value {
        serde_json::Value::String(value) => Ok(value),
        serde_json::Value::Number(value) => Ok(value.to_string()),
        _ => anyhow::bail!("command arguments must be strings or numbers"),
    }
}

fn print_dynamic_help(path: &str, descriptor: &CommandDescriptor) {
    println!("{path} — {}", descriptor.title);
    println!("{}", descriptor.description);
    for argument in &descriptor.arguments.arguments {
        let value = match argument.value_type {
            ValueType::String => "STRING",
            ValueType::Integer => "INTEGER",
            ValueType::Number => "NUMBER",
        };
        if argument.choices.is_empty() {
            println!("  --{} {value}", argument.name.replace('_', "-"));
        } else {
            println!(
                "  --{} <{}>",
                argument.name.replace('_', "-"),
                argument.choices.join("|")
            );
        }
    }
    if descriptor.target.is_some() {
        println!("  --target HANDLE@GENERATION");
    }
    if matches!(
        descriptor.mutation,
        bootty_app::commands::MutationClass::Destructive
    ) {
        println!("  --yes");
    }
}

fn print_command_response(response: control::RpcResponse, json_output: bool) -> Result<()> {
    let outcome = response
        .result
        .as_ref()
        .map(|value| serde_json::from_value::<bootty_app::commands::CommandOutcome>(value.clone()))
        .transpose()?;
    print_control_response(response, json_output)?;
    match outcome {
        None | Some(bootty_app::commands::CommandOutcome::Success { .. }) => Ok(()),
        Some(bootty_app::commands::CommandOutcome::Unsupported { message })
        | Some(bootty_app::commands::CommandOutcome::Unavailable { message }) => Err(CliFailure {
            code: EXIT_UNAVAILABLE,
            message,
        }
        .into()),
        Some(bootty_app::commands::CommandOutcome::Denied { message }) => Err(CliFailure {
            code: EXIT_DENIED,
            message,
        }
        .into()),
        Some(bootty_app::commands::CommandOutcome::StaleTarget { message }) => Err(CliFailure {
            code: EXIT_STALE_TARGET,
            message,
        }
        .into()),
        Some(bootty_app::commands::CommandOutcome::Failed { message, .. }) => Err(CliFailure {
            code: EXIT_COMMAND_FAILED,
            message,
        }
        .into()),
        Some(bootty_app::commands::CommandOutcome::ConfirmationRequired { .. }) => {
            Err(CliFailure {
                code: EXIT_CONFIRMATION,
                message: "command requires confirmation".to_owned(),
            }
            .into())
        }
    }
}

fn print_control_response(response: control::RpcResponse, json_output: bool) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string(&response)?);
        if let Some(error) = response.error {
            return Err(rpc_failure(error));
        }
        return Ok(());
    }
    if let Some(error) = response.error {
        return Err(rpc_failure(error));
    }
    if let Some(result) = response.result {
        match result {
            serde_json::Value::Array(values) => {
                for value in values {
                    if let Some(id) = value.get("id").and_then(serde_json::Value::as_str) {
                        println!("{id}");
                    } else {
                        println!("{}", serde_json::to_string_pretty(&value)?);
                    }
                }
            }
            serde_json::Value::Null => {}
            value => println!("{}", serde_json::to_string_pretty(&value)?),
        }
    }
    Ok(())
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
    let link = directory.join(ApplicationIdentity::current().cli_name());
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
