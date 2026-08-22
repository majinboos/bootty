use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

use bootty_command::{
    ArgumentSchema, Caller, CommandDescriptor, CommandInvocation, CommandOutcome, CompactSchema,
    MutationClass, ResourceKind, ValueType,
};
use bootty_control::ControlCatalog;
use bootty_extension::{ExtensionCatalog, ExtensionInvocationSender};
use bootty_mux::controller::SpaceId;

use crate::{
    action_catalog::Command,
    app_actions::{KeybindAction, SidebarAction, keybind_action_for_name},
};

mod runtime;

pub(crate) use runtime::CommandRuntime;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExactMuxTarget {
    Binding(SpaceId),
    Session(SpaceId, String),
    Window(SpaceId, String, String),
    Pane(SpaceId, String, String, String),
}

impl ExactMuxTarget {
    pub(crate) fn window(scope: SpaceId, session_id: &str, window_id: &str) -> Self {
        Self::Window(scope, session_id.to_owned(), window_id.to_owned())
    }

    pub(crate) fn scope(&self) -> SpaceId {
        match self {
            Self::Binding(scope)
            | Self::Session(scope, ..)
            | Self::Window(scope, ..)
            | Self::Pane(scope, ..) => *scope,
        }
    }

    pub(crate) fn ids(&self) -> (Option<&str>, Option<&str>, Option<&str>) {
        match self {
            Self::Binding(_) => (None, None, None),
            Self::Session(_, session) => (Some(session), None, None),
            Self::Window(_, session, window) => (Some(session), Some(window), None),
            Self::Pane(_, session, window, pane) => (Some(session), Some(window), Some(pane)),
        }
    }
}

pub fn command_invocation_from_catalog(
    command: Command,
    caller: Caller,
) -> Option<CommandInvocation> {
    command
        .palette_action()
        .map(|action| CommandInvocation::from_action(action, caller))
}

#[derive(Clone, Debug, PartialEq)]
pub enum CoreCommandExecutor {
    Keybind(KeybindAction),
    Sidebar(SidebarAction),
    CurrentResource(ResourceKind),
    ReadTerminal,
}

#[derive(Clone, Debug)]
struct RegisteredCommand {
    descriptor: CommandDescriptor,
    executor: CommandExecutorResolver,
}

#[derive(Clone, Copy, Debug)]
enum CommandExecutorResolver {
    Keybind,
    Sidebar(SidebarAction),
    CurrentResource,
    ReadTerminal,
    WriteTerminal,
}

#[derive(Clone, Debug, Default)]
pub struct CommandRegistry {
    commands: BTreeMap<String, RegisteredCommand>,
}

impl CommandRegistry {
    pub fn core() -> &'static Self {
        static REGISTRY: OnceLock<CommandRegistry> = OnceLock::new();
        REGISTRY.get_or_init(Self::from_core_commands)
    }

    pub fn list(&self) -> impl Iterator<Item = &CommandDescriptor> {
        self.commands.values().map(|command| &command.descriptor)
    }

    pub fn describe(&self, id: &str) -> Option<&CommandDescriptor> {
        self.commands.get(id).map(|command| &command.descriptor)
    }

    pub fn palette_commands(&self) -> impl Iterator<Item = Command> + '_ {
        Command::all().filter(|command| {
            command.palette_action().is_some()
                && self
                    .describe(command.id())
                    .is_some_and(|descriptor| descriptor.palette)
        })
    }

    pub fn resolve(
        &self,
        invocation: CommandInvocation,
    ) -> Result<ResolvedCommandInvocation, CommandOutcome> {
        let Some(registered) = self.commands.get(&invocation.command) else {
            return Err(CommandOutcome::Failed {
                code: "unknown_command".to_owned(),
                message: format!("unknown command {}", invocation.command),
            });
        };
        let descriptor = registered.descriptor.clone();
        validate_arguments(&descriptor, &invocation.arguments)?;
        let executor = match registered.executor {
            CommandExecutorResolver::Keybind => {
                let Some(action) = keybind_action_for_name(&invocation.action_name()) else {
                    return Err(CommandOutcome::Unsupported {
                        message: format!("command {} has no app executor", invocation.command),
                    });
                };
                CoreCommandExecutor::Keybind(action)
            }
            CommandExecutorResolver::Sidebar(action) => CoreCommandExecutor::Sidebar(action),
            CommandExecutorResolver::CurrentResource => CoreCommandExecutor::CurrentResource(
                resource_kind(&invocation.arguments[0])
                    .expect("validated resource kind has a runtime value"),
            ),
            CommandExecutorResolver::ReadTerminal => CoreCommandExecutor::ReadTerminal,
            CommandExecutorResolver::WriteTerminal => CoreCommandExecutor::Keybind(
                KeybindAction::Write(invocation.arguments[0].as_bytes().to_vec()),
            ),
        };
        Ok(ResolvedCommandInvocation {
            descriptor,
            executor: CommandExecutor::Core(executor),
            invocation,
        })
    }

    fn from_core_commands() -> Self {
        let mut commands = BTreeMap::new();
        for command in Command::all() {
            let descriptor = command.descriptor();
            commands
                .entry(descriptor.id.clone())
                .and_modify(|existing: &mut RegisteredCommand| {
                    existing.descriptor.palette |= descriptor.palette;
                })
                .or_insert(RegisteredCommand {
                    descriptor,
                    executor: CommandExecutorResolver::Keybind,
                });
        }
        for action in SidebarAction::ALL {
            let descriptor = sidebar_descriptor(action);
            commands.insert(
                descriptor.id.clone(),
                RegisteredCommand {
                    descriptor,
                    executor: CommandExecutorResolver::Sidebar(action),
                },
            );
        }
        let resource_kind_choices = [
            "instance",
            "application_window",
            "binding",
            "session",
            "mux_window",
            "pane",
            "terminal",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        for (descriptor, executor) in [
            (
                CommandDescriptor {
                    id: "resource.current".to_owned(),
                    title: "Current Resource".to_owned(),
                    description: "Return the current opaque resource target.".to_owned(),
                    mutation: MutationClass::Read,
                    arguments: CompactSchema {
                        arguments: vec![ArgumentSchema {
                            name: "kind".to_owned(),
                            value_type: ValueType::String,
                            required: true,
                            choices: resource_kind_choices,
                            minimum: None,
                            maximum: None,
                        }],
                    },
                    target: None,
                    palette: false,
                },
                CommandExecutorResolver::CurrentResource,
            ),
            (
                CommandDescriptor {
                    id: "terminal.read".to_owned(),
                    title: "Read Terminal".to_owned(),
                    description: "Read the active terminal screen.".to_owned(),
                    mutation: MutationClass::Read,
                    arguments: CompactSchema::default(),
                    target: Some(ResourceKind::Terminal),
                    palette: false,
                },
                CommandExecutorResolver::ReadTerminal,
            ),
            (
                CommandDescriptor {
                    id: "terminal.write".to_owned(),
                    title: "Write Terminal".to_owned(),
                    description: "Write literal text to the active terminal.".to_owned(),
                    mutation: MutationClass::Write,
                    arguments: CompactSchema {
                        arguments: vec![argument("text", ValueType::String)],
                    },
                    target: Some(ResourceKind::Terminal),
                    palette: false,
                },
                CommandExecutorResolver::WriteTerminal,
            ),
        ] {
            commands.insert(
                descriptor.id.clone(),
                RegisteredCommand {
                    descriptor,
                    executor,
                },
            );
        }
        Self { commands }
    }
}

#[derive(Clone)]
pub enum CommandExecutor {
    Core(CoreCommandExecutor),
    Extension(ExtensionInvocationSender),
}

#[derive(Clone)]
pub struct ResolvedCommandInvocation {
    pub descriptor: CommandDescriptor,
    pub invocation: CommandInvocation,
    pub executor: CommandExecutor,
}

#[derive(Clone)]
pub struct CommandCatalog {
    core: &'static CommandRegistry,
    extensions: Arc<ExtensionCatalog>,
    control: Arc<ControlCatalog>,
}

impl std::fmt::Debug for CommandCatalog {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CommandCatalog")
            .field("core", &self.core)
            .finish_non_exhaustive()
    }
}

impl Default for CommandCatalog {
    fn default() -> Self {
        let core = CommandRegistry::core();
        let extensions = Arc::new(ExtensionCatalog::with_reserved_commands(
            core.list().map(|descriptor| descriptor.id.clone()),
        ));
        Self {
            core,
            control: Arc::new(ControlCatalog::new(
                core.list().cloned().collect(),
                Arc::clone(&extensions),
            )),
            extensions,
        }
    }
}

impl CommandCatalog {
    pub fn list(&self) -> Vec<CommandDescriptor> {
        self.control.list()
    }

    pub fn describe(&self, id: &str) -> Option<CommandDescriptor> {
        self.control.describe(id)
    }

    pub fn resolve(
        &self,
        invocation: CommandInvocation,
    ) -> Result<ResolvedCommandInvocation, CommandOutcome> {
        if self.core.describe(&invocation.command).is_none()
            && let Some((descriptor, handler)) = self.extensions.command(&invocation.command)
        {
            validate_arguments(&descriptor, &invocation.arguments)?;
            return Ok(ResolvedCommandInvocation {
                descriptor,
                invocation,
                executor: CommandExecutor::Extension(handler),
            });
        }
        self.core.resolve(invocation)
    }

    pub fn extensions(&self) -> &ExtensionCatalog {
        &self.extensions
    }

    pub fn extensions_arc(&self) -> Arc<ExtensionCatalog> {
        Arc::clone(&self.extensions)
    }

    pub fn control_catalog(&self) -> Arc<ControlCatalog> {
        Arc::clone(&self.control)
    }
}

fn sidebar_descriptor(action: SidebarAction) -> CommandDescriptor {
    let (title, description) = match action {
        SidebarAction::Ignore => (
            "Ignore Sidebar Input",
            "Consume a sidebar key without changing the workspace.",
        ),
        SidebarAction::PreviousSession => (
            "Previous Sidebar Session",
            "Move the sidebar session cursor to the previous session.",
        ),
        SidebarAction::NextSession => (
            "Next Sidebar Session",
            "Move the sidebar session cursor to the next session.",
        ),
        SidebarAction::ActivateSession => (
            "Activate Sidebar Session",
            "Open the sidebar session under the cursor.",
        ),
        SidebarAction::FocusTerminal => (
            "Focus Terminal",
            "Return keyboard focus from the sidebar to the terminal.",
        ),
    };
    CommandDescriptor {
        id: action.command_id().to_owned(),
        title: title.to_owned(),
        description: description.to_owned(),
        mutation: MutationClass::Write,
        arguments: CompactSchema::default(),
        target: Some(ResourceKind::ApplicationWindow),
        palette: false,
    }
}

fn validate_arguments(
    descriptor: &CommandDescriptor,
    arguments: &[String],
) -> Result<(), CommandOutcome> {
    let required = descriptor
        .arguments
        .arguments
        .iter()
        .filter(|argument| argument.required)
        .count();
    if arguments.len() < required || arguments.len() > descriptor.arguments.arguments.len() {
        return Err(CommandOutcome::Failed {
            code: "invalid_arguments".to_owned(),
            message: format!(
                "command {} expects {} argument(s), got {}",
                descriptor.id,
                descriptor.arguments.arguments.len(),
                arguments.len()
            ),
        });
    }
    for (schema, value) in descriptor.arguments.arguments.iter().zip(arguments) {
        let valid_type = match schema.value_type {
            ValueType::String => true,
            ValueType::Integer => value.parse::<i64>().is_ok(),
            ValueType::Number => value.parse::<f32>().is_ok_and(f32::is_finite),
        };
        let parsed_integer = || value.parse::<i64>().ok();
        let valid_minimum = schema
            .minimum
            .is_none_or(|minimum| parsed_integer().is_some_and(|value| value >= minimum));
        let valid_maximum = schema
            .maximum
            .is_none_or(|maximum| parsed_integer().is_some_and(|value| value <= maximum));
        let valid = valid_type
            && valid_minimum
            && valid_maximum
            && (schema.choices.is_empty() || schema.choices.contains(value));
        if !valid {
            return Err(CommandOutcome::Failed {
                code: "invalid_arguments".to_owned(),
                message: format!("invalid {} argument for {}", schema.name, descriptor.id),
            });
        }
    }
    Ok(())
}

fn argument(name: &str, value_type: ValueType) -> ArgumentSchema {
    ArgumentSchema {
        name: name.to_owned(),
        value_type,
        required: true,
        choices: Vec::new(),
        minimum: None,
        maximum: None,
    }
}
fn resource_kind(value: &str) -> Option<ResourceKind> {
    match value {
        "instance" => Some(ResourceKind::Instance),
        "application_window" => Some(ResourceKind::ApplicationWindow),
        "binding" => Some(ResourceKind::Binding),
        "session" => Some(ResourceKind::Session),
        "mux_window" => Some(ResourceKind::MuxWindow),
        "pane" => Some(ResourceKind::Pane),
        "terminal" => Some(ResourceKind::Terminal),
        _ => None,
    }
}
