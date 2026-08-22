use std::io::Read;

use anyhow::{Context, Result};
use bootty_cli::Cli;
use bootty_command::{
    Caller, CommandDescriptor, CommandInvocation, CommandOutcome, CommandTarget, MutationClass,
    ValueType,
};
use bootty_control as control;
use thiserror::Error;

const EXIT_USAGE: u8 = 2;
const EXIT_TRANSPORT: u8 = 4;
const EXIT_CONFIRMATION: u8 = 5;
const EXIT_DENIED: u8 = 6;
const EXIT_UNAVAILABLE: u8 = 7;
const EXIT_STALE_TARGET: u8 = 8;
const EXIT_COMMAND_FAILED: u8 = 9;

#[derive(Debug, Error)]
#[error("{message}")]
struct CliFailure {
    code: u8,
    message: String,
}

pub(crate) fn exit_code(error: &anyhow::Error) -> u8 {
    error
        .downcast_ref::<CliFailure>()
        .map_or(1, |failure| failure.code)
}

pub(crate) fn invoke_dynamic_command(cli: &Cli, raw: &[String]) -> Result<()> {
    let (path, raw_arguments) = raw.split_first().context("missing command path")?;
    let name = path
        .split('.')
        .map(|segment| segment.replace('-', "_"))
        .collect::<Vec<_>>()
        .join(".");
    let instance =
        control::select_or_start(cli.start()).map_err(|error| transport_failure(&error))?;
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
        return Err(rpc_failure(&error));
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
    let (arguments, target, confirmed, detached) =
        parse_dynamic_arguments(&descriptor, raw_arguments)?;
    let invocation = CommandInvocation {
        command: descriptor.id,
        arguments,
        caller: Caller::Cli,
        target,
        confirmation: None,
    };
    let response = invoke_control_command_on_instance(&instance, invocation, confirmed, detached)?;
    if detached {
        print_control_response(response, cli.json())
    } else {
        print_command_response(response, cli.json())
    }
}

pub(crate) fn invoke_control_command(
    cli: &Cli,
    invocation: CommandInvocation,
    confirm: bool,
    detached: bool,
) -> Result<control::RpcResponse> {
    let descriptor =
        control::select_or_start(cli.start()).map_err(|error| transport_failure(&error))?;
    invoke_control_command_on_instance(&descriptor, invocation, confirm, detached)
}

fn invoke_control_command_on_instance(
    descriptor: &control::InstanceDescriptor,
    mut invocation: CommandInvocation,
    confirm: bool,
    detached: bool,
) -> Result<control::RpcResponse> {
    let response = invoke_command_request(descriptor, &invocation, detached && !confirm)?;
    if !confirm {
        return Ok(response);
    }
    let Some(result) = response.result.as_ref() else {
        return Ok(response);
    };
    let outcome = serde_json::from_value::<CommandOutcome>(result.clone())?;
    let CommandOutcome::ConfirmationRequired { confirmation } = outcome else {
        return Ok(response);
    };
    invocation.confirmation = Some(*confirmation);
    invoke_command_request(descriptor, &invocation, detached)
}

fn invoke_command_request(
    descriptor: &control::InstanceDescriptor,
    invocation: &CommandInvocation,
    detached: bool,
) -> Result<control::RpcResponse> {
    control_instance_request(
        descriptor,
        "command.invoke",
        serde_json::json!({"invocation": invocation, "detached": detached}),
    )
}

fn control_instance_request(
    descriptor: &control::InstanceDescriptor,
    method: &str,
    params: serde_json::Value,
) -> Result<control::RpcResponse> {
    control::invoke_instance(descriptor, method, params).map_err(|error| transport_failure(&error))
}

pub(crate) fn control_request(
    start: bool,
    method: &str,
    params: serde_json::Value,
) -> Result<control::RpcResponse> {
    control::invoke_or_start(start, method, params).map_err(|error| transport_failure(&error))
}

fn transport_failure(error: &anyhow::Error) -> anyhow::Error {
    CliFailure {
        code: EXIT_TRANSPORT,
        message: error.to_string(),
    }
    .into()
}

fn rpc_failure(error: &control::RpcError) -> anyhow::Error {
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
) -> Result<(Vec<String>, Option<CommandTarget>, bool, bool)> {
    let mut arguments = Vec::new();
    let mut target = None;
    let mut confirmed = false;
    let mut detached = false;
    let mut input = raw.iter();
    while let Some(argument) = input.next() {
        match argument.as_str() {
            "--yes" => confirmed = true,
            "--detach" => detached = true,
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
                let expected_name = expected.name.replace('_', "-");
                if expected_name != name {
                    anyhow::bail!("expected --{expected_name}, got {option}");
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
    Ok((arguments, target, confirmed, detached))
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
    if matches!(descriptor.mutation, MutationClass::Destructive) {
        println!("  --yes");
    }
    println!("  --detach");
}

pub(crate) fn print_command_response(
    response: control::RpcResponse,
    json_output: bool,
) -> Result<()> {
    let outcome = response
        .result
        .as_ref()
        .map(|value| serde_json::from_value::<CommandOutcome>(value.clone()))
        .transpose()?;
    print_control_response(response, json_output)?;
    let (code, message) = match outcome {
        None | Some(CommandOutcome::Success { .. }) => return Ok(()),
        Some(CommandOutcome::Unsupported { message } | CommandOutcome::Unavailable { message }) => {
            (EXIT_UNAVAILABLE, message)
        }
        Some(CommandOutcome::Denied { message }) => (EXIT_DENIED, message),
        Some(CommandOutcome::StaleTarget { message }) => (EXIT_STALE_TARGET, message),
        Some(CommandOutcome::Failed { message, .. }) => (EXIT_COMMAND_FAILED, message),
        Some(CommandOutcome::ConfirmationRequired { .. }) => (
            EXIT_CONFIRMATION,
            "command requires confirmation".to_owned(),
        ),
    };
    Err(CliFailure { code, message }.into())
}

pub(crate) fn print_control_response(
    response: control::RpcResponse,
    json_output: bool,
) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string(&response)?);
    }
    if let Some(error) = response.error {
        return Err(rpc_failure(&error));
    }
    if json_output {
        return Ok(());
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
