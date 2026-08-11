#![cfg_attr(windows, windows_subsystem = "windows")]

use std::{fmt, io::Read, process::ExitCode};

use anyhow::Result;
use bootty_app::{
    cli::{Cli, Command, RemoteSpaceCommand},
    commands::{
        ArgumentSchema, Caller, CommandDescriptor, CommandInvocation, CommandTarget, ResourceKind,
        ValueType,
    },
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
                control_request(&cli, cli.start(), "command.list", serde_json::Value::Null)?,
                cli.json(),
            )?;
            return Ok(());
        }
        Some(Command::Describe { name }) => {
            print_control_response(
                control_request(
                    &cli,
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
    if raw.is_empty() {
        return Err(usage_failure("missing command path"));
    }
    let instance =
        control::select_or_start(cli.instance(), cli.start()).map_err(transport_failure)?;
    let descriptors = dynamic_command_descriptors(&instance)?;
    let (descriptor, command, path_len) =
        if let Some((command, path_len)) = resolve_dynamic_command(&descriptors, raw) {
            (
                describe_dynamic_command(&instance, &command)?,
                command,
                path_len,
            )
        } else {
            resolve_dynamic_alias(&instance, raw)?
                .ok_or_else(|| usage_failure(format!("unknown command {}", raw[0])))?
        };
    let raw_arguments = &raw[path_len..];
    if raw_arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        print_dynamic_help(&raw[..path_len].join(" "), &descriptor);
        return Ok(());
    }
    let (mut values, target, confirmed) =
        parse_dynamic_argument_values(&descriptor, raw_arguments)?;
    complete_dynamic_argument_values(&descriptor, &mut values)?;
    match control::direct_control_request(&descriptor.id, &values)
        .map_err(|error| usage_failure(error.to_string()))?
    {
        Some(control::DirectControlRequest::CommandInvocation(invocation)) => {
            print_command_response(
                invoke_control_command_on_instance(&instance, invocation, confirmed)?,
                cli.json(),
            )
        }
        Some(control::DirectControlRequest::Rpc { method, params }) => {
            if confirmed {
                return Err(usage_failure(format!(
                    "command {} does not support --yes",
                    descriptor.id
                )));
            }
            if target.is_some() {
                return Err(usage_failure(format!(
                    "command {} does not support --target",
                    descriptor.id
                )));
            }
            print_control_response(
                control_instance_request(&instance, method, params)?,
                cli.json(),
            )
        }
        None => {
            let invocation = CommandInvocation {
                command,
                arguments: flatten_dynamic_arguments(&descriptor, values)?,
                caller: Caller::Cli,
                target,
                confirmation: None,
            };
            print_command_response(
                invoke_control_command_on_instance(&instance, invocation, confirmed)?,
                cli.json(),
            )
        }
    }
}

fn dynamic_command_descriptors(
    instance: &control::InstanceDescriptor,
) -> Result<Vec<CommandDescriptor>> {
    let response = control_instance_request(instance, "command.list", serde_json::Value::Null)?;
    if let Some(error) = response.error {
        return Err(rpc_failure(error));
    }
    let result = response.result.ok_or_else(|| {
        transport_failure(anyhow::anyhow!("command list response contained no result"))
    })?;
    serde_json::from_value(result).map_err(|error| transport_failure(error.into()))
}

fn describe_dynamic_command(
    instance: &control::InstanceDescriptor,
    command: &str,
) -> Result<CommandDescriptor> {
    let response = control_instance_request(
        instance,
        "command.describe",
        serde_json::json!({"command": command}),
    )?;
    if let Some(error) = response.error {
        return Err(rpc_failure(error));
    }
    let result = response.result.ok_or_else(|| {
        transport_failure(anyhow::anyhow!(
            "command descriptor response contained no result"
        ))
    })?;
    serde_json::from_value(result).map_err(|error| transport_failure(error.into()))
}

fn resolve_dynamic_command(
    descriptors: &[CommandDescriptor],
    raw: &[String],
) -> Option<(String, usize)> {
    dynamic_command_candidates(raw)
        .into_iter()
        .find_map(|(candidate, path_len)| {
            descriptors.iter().find_map(|descriptor| {
                dynamic_descriptor_command_name(descriptor, &candidate)
                    .map(|command| (command.to_owned(), path_len))
            })
        })
}

fn dynamic_descriptor_command_name<'a>(
    descriptor: &'a CommandDescriptor,
    candidate: &str,
) -> Option<&'a str> {
    if normalized_dynamic_name(&descriptor.id) == candidate {
        return Some(&descriptor.id);
    }
    descriptor
        .aliases
        .iter()
        .find(|alias| normalized_dynamic_name(alias) == candidate)
        .map(String::as_str)
}

fn resolve_dynamic_alias(
    instance: &control::InstanceDescriptor,
    raw: &[String],
) -> Result<Option<(CommandDescriptor, String, usize)>> {
    for (candidate, path_len) in dynamic_command_candidates(raw) {
        let response = control_instance_request(
            instance,
            "command.describe",
            serde_json::json!({"command": candidate}),
        )?;
        if let Some(error) = response.error {
            if error.code == -32602 {
                continue;
            }
            return Err(rpc_failure(error));
        }
        let result = response.result.ok_or_else(|| {
            transport_failure(anyhow::anyhow!(
                "command descriptor response contained no result"
            ))
        })?;
        let descriptor =
            serde_json::from_value(result).map_err(|error| transport_failure(error.into()))?;
        return Ok(Some((descriptor, candidate, path_len)));
    }
    Ok(None)
}

fn dynamic_command_candidates(raw: &[String]) -> Vec<(String, usize)> {
    let path_limit = raw
        .iter()
        .position(|argument| argument.as_str() == "--" || argument.starts_with("--"))
        .unwrap_or(raw.len());
    let mut candidates = (1..=path_limit)
        .rev()
        .map(|path_len| (normalized_dynamic_path(&raw[..path_len]), path_len))
        .collect::<Vec<_>>();
    let legacy_leaf = raw.first().and_then(|first| {
        normalized_dynamic_name(first)
            .rsplit_once('.')
            .map(|(_, leaf)| leaf.to_owned())
    });
    if let Some(leaf) = legacy_leaf
        && !candidates
            .iter()
            .any(|(candidate, path_len)| *path_len == 1 && candidate == &leaf)
    {
        candidates.push((leaf, 1));
    }
    candidates
}

fn normalized_dynamic_path(parts: &[String]) -> String {
    parts
        .iter()
        .flat_map(|part| part.split('.'))
        .map(|part| part.replace('-', "_"))
        .collect::<Vec<_>>()
        .join(".")
}

fn normalized_dynamic_name(name: &str) -> String {
    name.split('.')
        .map(|part| part.replace('-', "_"))
        .collect::<Vec<_>>()
        .join(".")
}

fn invoke_control_command(
    cli: &Cli,
    invocation: CommandInvocation,
    confirm: bool,
) -> Result<control::RpcResponse> {
    let descriptor =
        control::select_or_start(cli.instance(), cli.start()).map_err(transport_failure)?;
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
    let outcome = serde_json::from_value::<bootty_app::commands::CommandOutcome>(result.clone())
        .map_err(|error| transport_failure(error.into()))?;
    let bootty_app::commands::CommandOutcome::ConfirmationRequired { confirmation } = outcome
    else {
        return Ok(response);
    };
    apply_confirmation_replay(&mut invocation, *confirmation);
    control_instance_request(
        descriptor,
        "command.invoke",
        serde_json::json!({"invocation": invocation}),
    )
}

/// Replace the original request with the server-issued confirmation shape.
///
/// Some destructive operations bind an opaque argument (for example a
/// worktree-removal claimant set) only after their preflight. Replaying the
/// original CLI arguments would discard that binding.
fn apply_confirmation_replay(
    invocation: &mut CommandInvocation,
    confirmation: bootty_app::commands::Confirmation,
) {
    invocation.command = confirmation.command.clone();
    invocation.arguments = confirmation.arguments.clone();
    invocation.target = confirmation.target.clone();
    invocation.confirmation = Some(confirmation);
}

fn control_instance_request(
    descriptor: &control::InstanceDescriptor,
    method: &str,
    params: serde_json::Value,
) -> Result<control::RpcResponse> {
    control::invoke_instance(descriptor, method, params).map_err(transport_failure)
}

fn control_request(
    cli: &Cli,
    start: bool,
    method: &str,
    params: serde_json::Value,
) -> Result<control::RpcResponse> {
    control::invoke_or_start(cli.instance(), start, method, params).map_err(transport_failure)
}

fn usage_failure(message: impl Into<String>) -> anyhow::Error {
    CliFailure {
        code: EXIT_USAGE,
        message: message.into(),
    }
    .into()
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

#[cfg(test)]
fn parse_dynamic_arguments(
    descriptor: &CommandDescriptor,
    raw: &[String],
) -> Result<(Vec<String>, Option<CommandTarget>, bool)> {
    let (mut values, target, confirmed) = parse_dynamic_argument_values(descriptor, raw)?;
    complete_dynamic_argument_values(descriptor, &mut values)?;
    Ok((
        flatten_dynamic_arguments(descriptor, values)?,
        target,
        confirmed,
    ))
}

fn parse_dynamic_argument_values(
    descriptor: &CommandDescriptor,
    raw: &[String],
) -> Result<(Vec<Vec<String>>, Option<CommandTarget>, bool)> {
    validate_dynamic_schema(descriptor)?;
    let mut values = vec![Vec::new(); descriptor.arguments.arguments.len()];
    let mut target = None;
    let mut confirmed = false;
    let mut stdin_seen = false;
    let mut options = true;
    let mut input = raw.iter();

    while let Some(argument) = input.next() {
        let option = argument.as_str();
        if options && option == "--" {
            options = false;
            continue;
        }
        if options {
            if option == "--yes" {
                confirmed = true;
                continue;
            }
            if option.starts_with("--yes=") {
                return Err(usage_failure("--yes does not accept a value"));
            }
            if option == "--target" || option.starts_with("--target=") {
                if control::is_direct_control_command(&descriptor.id) {
                    return Err(usage_failure(format!(
                        "command {} does not support --target; select its instance with --instance and use its named resource argument",
                        descriptor.id
                    )));
                }
                if target.is_some() {
                    return Err(usage_failure("--target may only be specified once"));
                }
                let value = if let Some(value) = option.strip_prefix("--target=") {
                    value.to_owned()
                } else {
                    input
                        .next()
                        .cloned()
                        .ok_or_else(|| usage_failure("--target requires HANDLE@GENERATION"))?
                };
                target = Some(parse_dynamic_target(descriptor, &value)?);
                continue;
            }
            if option == "--stdin-json" {
                if stdin_seen {
                    return Err(usage_failure("--stdin-json may only be specified once"));
                }
                stdin_seen = true;
                append_stdin_json(descriptor, &mut values, read_stdin_json()?)?;
                continue;
            }
            if option.starts_with("--stdin-json=") {
                return Err(usage_failure(
                    "--stdin-json reads its value from standard input",
                ));
            }
            if let Some(option_name) = option.strip_prefix("--") {
                let (name, inline_value) = option_name
                    .split_once('=')
                    .map_or((option_name, None), |(name, value)| (name, Some(value)));
                let index = descriptor
                    .arguments
                    .arguments
                    .iter()
                    .position(|schema| schema.name == name || schema.name.replace('_', "-") == name)
                    .ok_or_else(|| {
                        usage_failure(format!("unknown argument {option} for {}", descriptor.id))
                    })?;
                let value = if let Some(value) = inline_value {
                    value.to_owned()
                } else {
                    input
                        .next()
                        .cloned()
                        .ok_or_else(|| usage_failure(format!("{option} requires a value")))?
                };
                append_dynamic_value(descriptor, &mut values, index, value)?;
                continue;
            }
        }
        append_positional_dynamic_value(descriptor, &mut values, option.to_owned())?;
    }

    Ok((values, target, confirmed))
}

fn validate_dynamic_schema(descriptor: &CommandDescriptor) -> Result<()> {
    let argument_count = descriptor.arguments.arguments.len();
    if let Some(argument) =
        descriptor
            .arguments
            .arguments
            .iter()
            .enumerate()
            .find_map(|(index, argument)| {
                (argument.repeated && index + 1 != argument_count).then_some(argument)
            })
    {
        return Err(usage_failure(format!(
            "repeated argument --{} must be last for {}",
            argument.name.replace('_', "-"),
            descriptor.id
        )));
    }
    Ok(())
}

fn parse_dynamic_target(descriptor: &CommandDescriptor, value: &str) -> Result<CommandTarget> {
    let kind = descriptor.target.ok_or_else(|| {
        usage_failure(format!(
            "command {} does not accept a target",
            descriptor.id
        ))
    })?;
    let (handle, generation) = value
        .rsplit_once('@')
        .ok_or_else(|| usage_failure("--target requires HANDLE@GENERATION"))?;
    if handle.is_empty()
        || generation.is_empty()
        || !generation.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(usage_failure("--target requires HANDLE@GENERATION"));
    }
    let generation = generation
        .parse()
        .map_err(|_| usage_failure("invalid target generation"))?;
    Ok(CommandTarget {
        kind,
        handle: handle.to_owned(),
        generation,
    })
}

fn read_stdin_json() -> Result<serde_json::Value> {
    let stdin = std::io::stdin();
    read_bounded_json(stdin.lock())
}

fn read_bounded_json(input: impl Read) -> Result<serde_json::Value> {
    let mut bytes = Vec::new();
    let mut input = input.take(control::REQUEST_LIMIT.saturating_add(1));
    input
        .read_to_end(&mut bytes)
        .map_err(|error| usage_failure(format!("failed to read stdin JSON: {error}")))?;
    if bytes.len() as u64 > control::REQUEST_LIMIT {
        return Err(usage_failure(format!(
            "stdin JSON exceeds the {} byte control request limit",
            control::REQUEST_LIMIT
        )));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| usage_failure(format!("invalid stdin JSON: {error}")))
}

fn append_stdin_json(
    descriptor: &CommandDescriptor,
    values: &mut [Vec<String>],
    input: serde_json::Value,
) -> Result<()> {
    match input {
        serde_json::Value::Object(object) => {
            let named_argument_object = !object.is_empty()
                && descriptor.arguments.arguments.len() > 1
                && object.keys().all(|name| {
                    descriptor
                        .arguments
                        .arguments
                        .iter()
                        .any(|schema| schema.name == name.as_str())
                });
            if !named_argument_object
                && let Some(index) = direct_json_object_slot(descriptor, values)
            {
                return append_json_dynamic_value(
                    descriptor,
                    values,
                    index,
                    serde_json::Value::Object(object),
                );
            }
            for (name, value) in object {
                let index = descriptor
                    .arguments
                    .arguments
                    .iter()
                    .position(|schema| schema.name == name)
                    .ok_or_else(|| {
                        usage_failure(format!(
                            "stdin JSON contains unknown argument {name} for {}",
                            descriptor.id
                        ))
                    })?;
                append_json_named_value(descriptor, values, index, value)?;
            }
            Ok(())
        }
        serde_json::Value::Array(array) => {
            if let Some(index) = direct_json_array_slot(descriptor, values) {
                return append_json_dynamic_value(
                    descriptor,
                    values,
                    index,
                    serde_json::Value::Array(array),
                );
            }
            for value in array {
                append_json_positional_value(descriptor, values, value)?;
            }
            Ok(())
        }
        value => append_json_positional_value(descriptor, values, value),
    }
}

fn direct_json_object_slot(
    descriptor: &CommandDescriptor,
    values: &[Vec<String>],
) -> Option<usize> {
    direct_json_slot(descriptor, values, |schema| {
        matches!(&schema.value_type, ValueType::Json | ValueType::Object)
    })
}

fn direct_json_array_slot(descriptor: &CommandDescriptor, values: &[Vec<String>]) -> Option<usize> {
    direct_json_slot(descriptor, values, |schema| {
        matches!(&schema.value_type, ValueType::Json | ValueType::Array) && !schema.repeated
    })
}

fn direct_json_slot(
    descriptor: &CommandDescriptor,
    values: &[Vec<String>],
    accepts: impl Fn(&ArgumentSchema) -> bool,
) -> Option<usize> {
    let schemas = &descriptor.arguments.arguments;
    schemas.iter().enumerate().find_map(|(index, schema)| {
        let preceding_values_are_present_or_defaulted = schemas[..index]
            .iter()
            .enumerate()
            .all(|(preceding, schema)| !values[preceding].is_empty() || schema.default.is_some());
        let following_required_values_are_present_or_defaulted = schemas[index + 1..]
            .iter()
            .enumerate()
            .all(|(offset, schema)| {
                !values[index + 1 + offset].is_empty()
                    || !schema.required
                    || schema.default.is_some()
            });
        (accepts(schema)
            && values[index].is_empty()
            && preceding_values_are_present_or_defaulted
            && following_required_values_are_present_or_defaulted)
            .then_some(index)
    })
}

fn append_json_named_value(
    descriptor: &CommandDescriptor,
    values: &mut [Vec<String>],
    index: usize,
    value: serde_json::Value,
) -> Result<()> {
    if descriptor.arguments.arguments[index].repeated
        && let serde_json::Value::Array(array) = value
    {
        for value in array {
            append_json_dynamic_value(descriptor, values, index, value)?;
        }
        return Ok(());
    }
    append_json_dynamic_value(descriptor, values, index, value)
}

fn append_json_positional_value(
    descriptor: &CommandDescriptor,
    values: &mut [Vec<String>],
    value: serde_json::Value,
) -> Result<()> {
    let index = next_dynamic_argument_index(descriptor, values)?;
    append_json_dynamic_value(descriptor, values, index, value)
}

fn append_json_dynamic_value(
    descriptor: &CommandDescriptor,
    values: &mut [Vec<String>],
    index: usize,
    value: serde_json::Value,
) -> Result<()> {
    let schema = descriptor
        .arguments
        .arguments
        .get(index)
        .ok_or_else(|| usage_failure("too many command arguments"))?;
    let value = json_argument(descriptor, schema, value)?;
    append_dynamic_value(descriptor, values, index, value)
}

fn json_argument(
    descriptor: &CommandDescriptor,
    schema: &ArgumentSchema,
    value: serde_json::Value,
) -> Result<String> {
    let argument = match &schema.value_type {
        ValueType::Null => {
            if !value.is_null() {
                return Err(invalid_json_argument(descriptor, schema));
            }
            "null".to_owned()
        }
        ValueType::String => match value {
            serde_json::Value::String(value) => value,
            serde_json::Value::Number(value) => value.to_string(),
            serde_json::Value::Bool(value) => value.to_string(),
            _ => return Err(invalid_json_argument(descriptor, schema)),
        },
        ValueType::Enum | ValueType::ResourceRef => match value {
            serde_json::Value::String(value) => value,
            _ => return Err(invalid_json_argument(descriptor, schema)),
        },
        ValueType::Integer | ValueType::Number => match value {
            serde_json::Value::String(value) => value,
            serde_json::Value::Number(value) => value.to_string(),
            _ => return Err(invalid_json_argument(descriptor, schema)),
        },
        ValueType::Boolean => match value {
            serde_json::Value::String(value) => value,
            serde_json::Value::Bool(value) => value.to_string(),
            _ => return Err(invalid_json_argument(descriptor, schema)),
        },
        ValueType::Json => serde_json::to_string(&value).map_err(|error| {
            usage_failure(format!(
                "cannot encode JSON argument --{} for {}: {error}",
                schema.name, descriptor.id
            ))
        })?,
        ValueType::Array => {
            if !value.is_array() {
                return Err(invalid_json_argument(descriptor, schema));
            }
            serde_json::to_string(&value).map_err(|error| {
                usage_failure(format!(
                    "cannot encode JSON argument --{} for {}: {error}",
                    schema.name, descriptor.id
                ))
            })?
        }
        ValueType::Object => {
            if !value.is_object() {
                return Err(invalid_json_argument(descriptor, schema));
            }
            serde_json::to_string(&value).map_err(|error| {
                usage_failure(format!(
                    "cannot encode JSON argument --{} for {}: {error}",
                    schema.name, descriptor.id
                ))
            })?
        }
    };
    validate_dynamic_value(descriptor, schema, &argument)?;
    Ok(argument)
}

fn invalid_json_argument(descriptor: &CommandDescriptor, schema: &ArgumentSchema) -> anyhow::Error {
    usage_failure(format!(
        "invalid JSON value for --{} in {}",
        schema.name.replace('_', "-"),
        descriptor.id
    ))
}

fn append_positional_dynamic_value(
    descriptor: &CommandDescriptor,
    values: &mut [Vec<String>],
    value: String,
) -> Result<()> {
    let index = next_dynamic_argument_index(descriptor, values)?;
    append_dynamic_value(descriptor, values, index, value)
}

fn next_dynamic_argument_index(
    descriptor: &CommandDescriptor,
    values: &[Vec<String>],
) -> Result<usize> {
    descriptor
        .arguments
        .arguments
        .iter()
        .enumerate()
        .find_map(|(index, schema)| (schema.repeated || values[index].is_empty()).then_some(index))
        .ok_or_else(|| usage_failure("too many command arguments"))
}

fn append_dynamic_value(
    descriptor: &CommandDescriptor,
    values: &mut [Vec<String>],
    index: usize,
    value: String,
) -> Result<()> {
    let schema = descriptor
        .arguments
        .arguments
        .get(index)
        .ok_or_else(|| usage_failure("too many command arguments"))?;
    if !schema.repeated && !values[index].is_empty() {
        return Err(usage_failure(format!(
            "--{} may only be specified once",
            schema.name.replace('_', "-")
        )));
    }
    validate_dynamic_value(descriptor, schema, &value)?;
    values[index].push(value);
    Ok(())
}

fn validate_dynamic_value(
    descriptor: &CommandDescriptor,
    schema: &ArgumentSchema,
    value: &str,
) -> Result<()> {
    let valid_type = match &schema.value_type {
        ValueType::Null => {
            serde_json::from_str::<serde_json::Value>(value).is_ok_and(|value| value.is_null())
        }
        ValueType::String => true,
        ValueType::Enum | ValueType::ResourceRef => !value.is_empty(),
        ValueType::Integer => value.parse::<i64>().is_ok(),
        ValueType::Number => value.parse::<f32>().is_ok_and(f32::is_finite),
        ValueType::Boolean => matches!(value, "true" | "false"),
        ValueType::Json => serde_json::from_str::<serde_json::Value>(value).is_ok(),
        ValueType::Array => {
            serde_json::from_str::<serde_json::Value>(value).is_ok_and(|value| value.is_array())
        }
        ValueType::Object => {
            serde_json::from_str::<serde_json::Value>(value).is_ok_and(|value| value.is_object())
        }
    };
    let parsed_integer = value.parse::<i64>().ok();
    let valid_minimum = schema
        .minimum
        .is_none_or(|minimum| parsed_integer.is_some_and(|value| value >= minimum));
    let valid_maximum = schema
        .maximum
        .is_none_or(|maximum| parsed_integer.is_some_and(|value| value <= maximum));
    let valid_choice =
        schema.choices.is_empty() || schema.choices.iter().any(|choice| choice == value);
    if valid_type && valid_minimum && valid_maximum && valid_choice {
        return Ok(());
    }
    Err(usage_failure(format!(
        "invalid {} argument for {}",
        schema.name, descriptor.id
    )))
}

#[cfg(test)]
fn complete_dynamic_arguments(
    descriptor: &CommandDescriptor,
    mut values: Vec<Vec<String>>,
) -> Result<Vec<String>> {
    complete_dynamic_argument_values(descriptor, &mut values)?;
    flatten_dynamic_arguments(descriptor, values)
}

fn complete_dynamic_argument_values(
    descriptor: &CommandDescriptor,
    values: &mut [Vec<String>],
) -> Result<()> {
    for index in 0..values.len() {
        if values[index].is_empty()
            && let Some(default) = descriptor.arguments.arguments[index].default.clone()
        {
            append_dynamic_value(descriptor, values, index, default)?;
        }
    }
    for (schema, values) in descriptor.arguments.arguments.iter().zip(&*values) {
        if schema.required && values.is_empty() {
            return Err(usage_failure(format!(
                "command {} requires --{}",
                descriptor.id,
                schema.name.replace('_', "-")
            )));
        }
    }
    Ok(())
}

fn flatten_dynamic_arguments(
    descriptor: &CommandDescriptor,
    values: Vec<Vec<String>>,
) -> Result<Vec<String>> {
    if let Some(last) = values.iter().rposition(|values| !values.is_empty())
        && let Some((_, schema)) = descriptor.arguments.arguments[..=last]
            .iter()
            .enumerate()
            .find(|(index, _)| values[*index].is_empty())
    {
        return Err(usage_failure(format!(
            "cannot omit --{} before later command arguments",
            schema.name.replace('_', "-")
        )));
    }
    Ok(values.into_iter().flatten().collect())
}

fn print_dynamic_help(path: &str, descriptor: &CommandDescriptor) {
    print!("{}", dynamic_help(path, descriptor));
}

fn dynamic_help(path: &str, descriptor: &CommandDescriptor) -> String {
    let mut help = format!(
        "{path} — {}\n{}\n\nUsage: bootty {path} [OPTIONS]\n\nOptions:\n",
        descriptor.title, descriptor.description
    );
    for argument in &descriptor.arguments.arguments {
        let mut details = vec![if argument.required {
            "required".to_owned()
        } else {
            "optional".to_owned()
        }];
        if argument.repeated {
            details.push("repeated".to_owned());
        }
        if let Some(default) = &argument.default {
            details.push(format!("default: {default:?}"));
        }
        if !argument.choices.is_empty() {
            details.push(format!("choices: {}", argument.choices.join("|")));
        }
        match (argument.minimum, argument.maximum) {
            (Some(minimum), Some(maximum)) => details.push(format!("range: {minimum}..={maximum}")),
            (Some(minimum), None) => details.push(format!("range: {minimum}..")),
            (None, Some(maximum)) => details.push(format!("range: ..={maximum}")),
            (None, None) => {}
        }
        help.push_str(&format!(
            "  --{} {}  {}\n",
            argument.name.replace('_', "-"),
            dynamic_value_type_name(&argument.value_type),
            details.join("; ")
        ));
    }
    if !descriptor.arguments.arguments.is_empty() {
        help.push_str(&format!(
            "  --stdin-json  read an object, array, or scalar JSON value from stdin (up to {} bytes)\n",
            control::REQUEST_LIMIT
        ));
    }
    if let Some(kind) = descriptor.target
        && !control::is_direct_control_command(&descriptor.id)
    {
        help.push_str(&format!(
            "  --target HANDLE@GENERATION  target: {}\n",
            dynamic_target_kind_name(kind)
        ));
    }
    if matches!(
        descriptor.mutation,
        bootty_app::commands::MutationClass::Destructive
    ) || descriptor.id == "command.invoke"
    {
        help.push_str("  --yes  confirm a destructive command\n");
    }
    help
}

fn dynamic_value_type_name(value_type: &ValueType) -> &'static str {
    match value_type {
        ValueType::Null => "NULL",
        ValueType::Boolean => "BOOLEAN",
        ValueType::Integer => "INTEGER",
        ValueType::Number => "NUMBER",
        ValueType::String => "STRING",
        ValueType::Enum => "ENUM",
        ValueType::Array => "ARRAY",
        ValueType::Object => "OBJECT",
        ValueType::ResourceRef => "RESOURCE_REF",
        ValueType::Json => "JSON",
    }
}

fn dynamic_target_kind_name(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::Instance => "instance",
        ResourceKind::ApplicationWindow => "application_window",
        ResourceKind::Binding => "binding",
        ResourceKind::Space => "space",
        ResourceKind::Session => "session",
        ResourceKind::MuxWindow => "mux_window",
        ResourceKind::Pane => "pane",
        ResourceKind::Terminal => "terminal",
        ResourceKind::Client => "client",
        ResourceKind::Directory => "directory",
        ResourceKind::Worktree => "worktree",
        ResourceKind::Task => "task",
        ResourceKind::Subscription => "subscription",
        ResourceKind::Surface => "surface",
        ResourceKind::Extension => "extension",
    }
}

fn print_command_response(response: control::RpcResponse, json_output: bool) -> Result<()> {
    let outcome = response
        .result
        .as_ref()
        .map(|value| serde_json::from_value::<bootty_app::commands::CommandOutcome>(value.clone()))
        .transpose()
        .map_err(|error| transport_failure(error.into()));
    if json_output {
        print_control_response(response, true)?;
        return finish_command_outcome(outcome?);
    }
    let outcome = outcome?;
    print_control_response(response, false)?;
    finish_command_outcome(outcome)
}

fn finish_command_outcome(outcome: Option<bootty_app::commands::CommandOutcome>) -> Result<()> {
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
        Some(bootty_app::commands::CommandOutcome::Pending { .. }) => Err(CliFailure {
            code: EXIT_COMMAND_FAILED,
            message: "command is still pending".to_owned(),
        }
        .into()),
        Some(bootty_app::commands::CommandOutcome::Ambiguous { message, .. })
        | Some(bootty_app::commands::CommandOutcome::Failed { message, .. }) => Err(CliFailure {
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

#[cfg(test)]
mod control_cli_tests {
    use std::io::Cursor;
    use std::path::PathBuf;

    use super::*;
    use bootty_app::{
        automation::{RepositoryRef, WorktreeRef, WorktreeRemovalConfirmation},
        commands::{CommandRegistry, CompactSchema, CoreCommandExecutor, MutationClass},
    };
    use serde_json::json;

    fn test_schema(name: &str, value_type: ValueType, required: bool) -> ArgumentSchema {
        ArgumentSchema {
            name: name.to_owned(),
            value_type,
            required,
            choices: Vec::new(),
            minimum: None,
            maximum: None,
            default: None,
            repeated: false,
        }
    }

    fn test_descriptor(id: &str, arguments: Vec<ArgumentSchema>) -> CommandDescriptor {
        CommandDescriptor {
            id: id.to_owned(),
            title: "Test command".to_owned(),
            description: "Test command description.".to_owned(),
            aliases: Vec::new(),
            origin: None,
            mutation: MutationClass::Write,
            arguments: CompactSchema { arguments },
            result_schema: None,
            targets: Vec::new(),
            availability: None,
            target: None,
            palette: false,
            palette_metadata: None,
        }
    }

    fn assert_usage(error: anyhow::Error) {
        assert_eq!(
            error
                .downcast_ref::<CliFailure>()
                .map(|failure| failure.code),
            Some(EXIT_USAGE)
        );
    }

    fn dynamic_direct_request(id: &str, raw: &[&str]) -> control::DirectControlRequest {
        let descriptor = CommandRegistry::core()
            .describe(id)
            .expect("catalog direct-control descriptor");
        let raw = raw
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect::<Vec<_>>();
        let (mut values, target, confirmed) =
            parse_dynamic_argument_values(&descriptor, &raw).expect("dynamic arguments");
        complete_dynamic_argument_values(&descriptor, &mut values)
            .expect("complete dynamic arguments");
        assert!(target.is_none());
        assert!(!confirmed);
        control::direct_control_request(&descriptor.id, &values)
            .expect("valid direct-control request")
            .expect("catalog direct-control request")
    }

    #[test]
    fn dynamic_arguments_follow_descriptor_schema() {
        let mut delta = test_schema("delta", ValueType::Integer, true);
        delta.minimum = Some(-4);
        delta.maximum = Some(4);
        let mut descriptor = test_descriptor("scroll_page_lines", vec![delta]);
        descriptor.target = Some(ResourceKind::Terminal);

        let (arguments, target, confirmed) = parse_dynamic_arguments(
            &descriptor,
            &[
                "--delta".to_owned(),
                "-3".to_owned(),
                "--target".to_owned(),
                "binding:1/terminal:2@4".to_owned(),
            ],
        )
        .unwrap();

        assert_eq!(arguments, ["-3"]);
        assert_eq!(target.unwrap().generation, 4);
        assert!(!confirmed);
    }

    #[test]
    fn dynamic_event_and_task_commands_use_direct_rpc_shapes() {
        let subscription = dynamic_direct_request(
            "event.subscribe",
            &[
                "--topics",
                r#"["terminal.output"]"#,
                "--scope",
                "binding:1:2",
            ],
        );
        let control::DirectControlRequest::Rpc { method, params } = subscription else {
            panic!("event.subscribe must bypass command.invoke");
        };
        assert_eq!(method, "event.subscribe");
        assert_eq!(
            params,
            json!({
                "topics": ["terminal.output"],
                "scope": "binding:1:2",
            })
        );

        let initial_poll = dynamic_direct_request(
            "event.subscribe",
            &["--subscription", "opaque-subscription"],
        );
        let control::DirectControlRequest::Rpc { method, params } = initial_poll else {
            panic!("event.subscribe polling must bypass command.invoke");
        };
        assert_eq!(method, "event.subscribe");
        assert_eq!(params, json!({"subscription": "opaque-subscription"}));

        let poll = dynamic_direct_request(
            "event.subscribe",
            &["--subscription", "opaque-subscription", "--cursor", "42"],
        );
        let control::DirectControlRequest::Rpc { method, params } = poll else {
            panic!("event.subscribe polling must bypass command.invoke");
        };
        assert_eq!(method, "event.subscribe");
        assert_eq!(
            params,
            json!({"subscription": "opaque-subscription", "cursor": 42})
        );

        for (id, option, expected_method) in [
            ("event.snapshot", "--subscription", "event.snapshot"),
            ("event.rebase", "--subscription", "event.rebase"),
            ("event.unsubscribe", "--subscription", "event.unsubscribe"),
            ("task.status", "--task", "task.status"),
            ("task.cancel", "--task", "task.cancel"),
        ] {
            let request = dynamic_direct_request(id, &[option, "opaque-resource"]);
            let control::DirectControlRequest::Rpc { method, params } = request else {
                panic!("{id} must bypass command.invoke");
            };
            assert_eq!(method, expected_method);
            let parameter = if option == "--task" {
                json!({"task": "opaque-resource"})
            } else {
                json!({"subscription": "opaque-resource"})
            };
            assert_eq!(params, parameter);
        }

        let task = CommandRegistry::core()
            .describe("task.cancel")
            .expect("task.cancel descriptor");
        assert_usage(
            parse_dynamic_arguments(
                &task,
                &[
                    "--task".to_owned(),
                    "opaque-resource".to_owned(),
                    "--target".to_owned(),
                    "task:stale@1".to_owned(),
                ],
            )
            .unwrap_err(),
        );
    }

    #[test]
    fn dynamic_ping_preserves_an_independent_maximum_protocol_version() {
        let ping = dynamic_direct_request("system.ping", &["--maximum-protocol-version", "7"]);
        let control::DirectControlRequest::Rpc { method, params } = ping else {
            panic!("system.ping must bypass command.invoke");
        };
        assert_eq!(method, "system.ping");
        assert_eq!(params, json!({"maximum_protocol_version": 7}));
    }

    #[test]
    fn optional_copy_format_can_be_omitted() {
        let descriptor = CommandRegistry::core()
            .describe("copy_to_clipboard")
            .expect("clipboard alias is registered");
        assert_eq!(descriptor.id, "clipboard.copy");
        assert_eq!(descriptor.arguments.arguments[0].name, "format");
        assert!(!descriptor.arguments.arguments[0].required);

        let (arguments, target, confirmed) = parse_dynamic_arguments(&descriptor, &[]).unwrap();

        assert!(arguments.is_empty());
        assert!(target.is_none());
        assert!(!confirmed);
    }

    #[test]
    fn invalid_choices_and_ranges_are_usage_errors_before_rpc() {
        let copy = CommandRegistry::core()
            .describe("copy_to_clipboard")
            .expect("clipboard alias is registered");
        assert_usage(parse_dynamic_arguments(&copy, &["unknown".to_owned()]).unwrap_err());

        let mut delta = test_schema("delta", ValueType::Integer, true);
        delta.minimum = Some(i64::from(i16::MIN));
        delta.maximum = Some(i64::from(i16::MAX));
        let scroll = test_descriptor("scroll_page_lines", vec![delta]);
        assert_usage(parse_dynamic_arguments(&scroll, &["32768".to_owned()]).unwrap_err());
    }

    #[test]
    fn repeated_and_default_values_round_trip() {
        let mut format = test_schema("format", ValueType::String, false);
        format.default = Some("plain".to_owned());
        let mut label = test_schema("label", ValueType::String, false);
        label.repeated = true;
        let descriptor = test_descriptor("agents.prompt", vec![format, label]);

        let (arguments, target, confirmed) = parse_dynamic_arguments(
            &descriptor,
            &[
                "--label".to_owned(),
                "first".to_owned(),
                "--label".to_owned(),
                "second".to_owned(),
            ],
        )
        .unwrap();

        assert_eq!(arguments, ["plain", "first", "second"]);
        assert!(target.is_none());
        assert!(!confirmed);
    }

    #[test]
    fn stdin_json_uses_descriptor_shaped_object_array_and_scalar_values() {
        let object_descriptor = test_descriptor(
            "agents.ingest",
            vec![test_schema("payload", ValueType::Object, true)],
        );
        let mut object_values = vec![Vec::new()];
        append_stdin_json(
            &object_descriptor,
            &mut object_values,
            json!({"agent": "codex"}),
        )
        .unwrap();
        assert_eq!(
            complete_dynamic_arguments(&object_descriptor, object_values).unwrap(),
            [r#"{"agent":"codex"}"#]
        );

        let array_descriptor = test_descriptor(
            "agents.prompt",
            vec![
                test_schema("agent", ValueType::String, true),
                test_schema("count", ValueType::Integer, true),
            ],
        );
        let mut array_values = vec![Vec::new(), Vec::new()];
        append_stdin_json(&array_descriptor, &mut array_values, json!(["codex", 2])).unwrap();
        assert_eq!(
            complete_dynamic_arguments(&array_descriptor, array_values).unwrap(),
            ["codex", "2"]
        );

        let mut format = test_schema("format", ValueType::String, false);
        format.default = Some("plain".to_owned());
        let named_object_descriptor = test_descriptor(
            "agents.prompt",
            vec![format, test_schema("payload", ValueType::Object, true)],
        );
        let mut named_object_values = vec![Vec::new(), Vec::new()];
        append_stdin_json(
            &named_object_descriptor,
            &mut named_object_values,
            json!({"format": "html", "payload": {"agent": "codex"}}),
        )
        .unwrap();
        assert_eq!(
            complete_dynamic_arguments(&named_object_descriptor, named_object_values).unwrap(),
            ["html", r#"{"agent":"codex"}"#]
        );

        let mut mode = test_schema("mode", ValueType::String, false);
        mode.default = Some("plain".to_owned());
        let defaulted_object_descriptor = test_descriptor(
            "agents.ingest",
            vec![mode, test_schema("payload", ValueType::Object, true)],
        );
        let mut defaulted_object_values = vec![Vec::new(), Vec::new()];
        append_stdin_json(
            &defaulted_object_descriptor,
            &mut defaulted_object_values,
            json!({}),
        )
        .unwrap();
        assert_eq!(
            complete_dynamic_arguments(&defaulted_object_descriptor, defaulted_object_values)
                .unwrap(),
            ["plain", "{}"]
        );

        let json_descriptor = test_descriptor(
            "agents.ingest",
            vec![test_schema("payload", ValueType::Json, true)],
        );
        let mut json_values = vec![Vec::new()];
        append_stdin_json(
            &json_descriptor,
            &mut json_values,
            json!([{"agent": "codex"}]),
        )
        .unwrap();
        assert_eq!(
            complete_dynamic_arguments(&json_descriptor, json_values).unwrap(),
            [r#"[{"agent":"codex"}]"#]
        );

        let scalar_descriptor = test_descriptor(
            "agents.enabled",
            vec![test_schema("enabled", ValueType::Boolean, true)],
        );
        let mut scalar_values = vec![Vec::new()];
        append_stdin_json(&scalar_descriptor, &mut scalar_values, json!(true)).unwrap();
        assert_eq!(
            complete_dynamic_arguments(&scalar_descriptor, scalar_values).unwrap(),
            ["true"]
        );
    }

    #[test]
    fn dynamic_schema_preserves_enum_array_resource_ref_and_null_types() {
        let mut direction = test_schema("direction", ValueType::Enum, true);
        direction.choices = vec!["next".to_owned(), "previous".to_owned()];
        let descriptor = test_descriptor(
            "event.rebase",
            vec![
                direction,
                test_schema("topics", ValueType::Array, true),
                test_schema("subscription", ValueType::ResourceRef, true),
                test_schema("marker", ValueType::Null, true),
            ],
        );

        let (arguments, _, _) = parse_dynamic_arguments(
            &descriptor,
            &[
                "--direction".to_owned(),
                "previous".to_owned(),
                "--topics".to_owned(),
                r#"["terminal.output"]"#.to_owned(),
                "--subscription".to_owned(),
                "subscription-1".to_owned(),
                "--marker".to_owned(),
                "null".to_owned(),
            ],
        )
        .expect("typed dynamic arguments");
        assert_eq!(
            arguments,
            vec![
                "previous".to_owned(),
                r#"["terminal.output"]"#.to_owned(),
                "subscription-1".to_owned(),
                "null".to_owned()
            ]
        );
        assert_usage(
            parse_dynamic_arguments(
                &descriptor,
                &[
                    "--direction".to_owned(),
                    "backward".to_owned(),
                    "--topics".to_owned(),
                    "[]".to_owned(),
                    "--subscription".to_owned(),
                    "subscription-1".to_owned(),
                    "--marker".to_owned(),
                    "null".to_owned(),
                ],
            )
            .unwrap_err(),
        );

        let help = dynamic_help("event rebase", &descriptor);
        for kind in ["ENUM", "ARRAY", "RESOURCE_REF", "NULL"] {
            assert!(help.contains(kind), "missing {kind} in {help}");
        }
    }

    #[test]
    fn stdin_json_is_bounded_by_the_control_request_limit() {
        let bytes = vec![b' '; control::REQUEST_LIMIT as usize + 1];

        assert_usage(read_bounded_json(Cursor::new(bytes)).unwrap_err());
    }

    #[test]
    fn target_selectors_preserve_opaque_handles_and_require_a_generation() {
        let mut descriptor = test_descriptor("terminal.read", Vec::new());
        descriptor.target = Some(ResourceKind::Terminal);

        let (_, target, _) = parse_dynamic_arguments(
            &descriptor,
            &["--target".to_owned(), "terminal:1@2@3".to_owned()],
        )
        .unwrap();
        let target = target.unwrap();
        assert_eq!(target.handle, "terminal:1@2");
        assert_eq!(target.generation, 3);

        assert_usage(
            parse_dynamic_arguments(
                &descriptor,
                &["--target".to_owned(), "terminal:1@".to_owned()],
            )
            .unwrap_err(),
        );
    }

    #[test]
    fn yes_replay_carries_bound_worktree_confirmation_arguments() {
        let worktree = WorktreeRef {
            repository: RepositoryRef {
                common_git_dir: PathBuf::from("/repo/.git"),
                root: Some(PathBuf::from("/repo")),
            },
            git_dir: PathBuf::from("/repo/.git/worktrees/feature"),
            path: PathBuf::from("/repo-feature"),
            branch: Some("feature".to_owned()),
            head: Some("deadbeef".to_owned()),
            created_by: None,
            managed_by_bootty: false,
        };
        let bound = WorktreeRemovalConfirmation {
            worktree: worktree.clone(),
            conflicting_claims: Vec::new(),
        };
        let mut invocation = CommandInvocation {
            command: "worktree.remove".to_owned(),
            arguments: vec![
                worktree.path.to_string_lossy().into_owned(),
                "false".to_owned(),
            ],
            caller: Caller::Cli,
            target: None,
            confirmation: None,
        };
        apply_confirmation_replay(
            &mut invocation,
            bootty_app::commands::Confirmation {
                command: "worktree.remove".to_owned(),
                arguments: vec![
                    worktree.path.to_string_lossy().into_owned(),
                    "true".to_owned(),
                    serde_json::to_string(&bound).expect("encode bound confirmation"),
                ],
                target: None,
            },
        );

        let resolved = CommandRegistry::core()
            .resolve(invocation)
            .expect("the --yes replay must resolve with the server-issued arguments");
        assert!(matches!(
            resolved.executor,
            CoreCommandExecutor::WorktreeRemove {
                path,
                force: true,
                confirmation: Some(confirmation),
            } if path == worktree.path.to_string_lossy() && confirmation == bound
        ));
    }

    #[test]
    fn dynamic_command_resolution_uses_the_longest_discovered_path() {
        let mut clipboard = test_descriptor("clipboard.copy", Vec::new());
        clipboard.aliases = vec!["copy_to_clipboard".to_owned()];
        let descriptors = vec![
            test_descriptor("agents", Vec::new()),
            test_descriptor("agents.prompt", Vec::new()),
            test_descriptor("scroll_page_lines", Vec::new()),
            clipboard,
        ];
        let words = [
            "agents".to_owned(),
            "prompt".to_owned(),
            "--message".to_owned(),
            "hello".to_owned(),
        ];
        let (command, path_len) = resolve_dynamic_command(&descriptors, &words).unwrap();

        assert_eq!(command, "agents.prompt");
        assert_eq!(path_len, 2);

        let legacy = ["terminal.scroll-page-lines".to_owned()];
        let (command, path_len) = resolve_dynamic_command(&descriptors, &legacy).unwrap();
        assert_eq!(command, "scroll_page_lines");
        assert_eq!(path_len, 1);

        let alias = ["copy-to-clipboard".to_owned()];
        let (command, path_len) = resolve_dynamic_command(&descriptors, &alias).unwrap();
        assert_eq!(command, "copy_to_clipboard");
        assert_eq!(path_len, 1);
    }

    #[test]
    fn generated_help_includes_descriptor_metadata() {
        let mut count = test_schema("count", ValueType::Integer, true);
        count.choices = vec!["1".to_owned(), "2".to_owned()];
        count.minimum = Some(1);
        count.maximum = Some(2);
        let mut label = test_schema("label", ValueType::String, false);
        label.default = Some("all".to_owned());
        label.repeated = true;
        let mut descriptor = test_descriptor("agents.prompt", vec![count, label]);
        descriptor.target = Some(ResourceKind::Session);
        descriptor.mutation = MutationClass::Destructive;

        let help = dynamic_help("agents prompt", &descriptor);

        for expected in [
            "required",
            "optional",
            "repeated",
            r#"default: "all""#,
            "choices: 1|2",
            "range: 1..=2",
            "--stdin-json",
            "--target HANDLE@GENERATION",
            "target: session",
            "--yes",
        ] {
            assert!(help.contains(expected), "missing {expected:?} in {help}");
        }
    }

    #[test]
    fn rpc_errors_have_stable_exit_categories() {
        let error = rpc_failure(control::RpcError {
            code: -32006,
            message: "denied".to_owned(),
            data: None,
        });

        assert_eq!(
            error.downcast_ref::<CliFailure>().unwrap().code,
            EXIT_DENIED
        );
    }
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
