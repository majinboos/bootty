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

use crate::{
    action_catalog::Command,
    app_actions::{KeybindAction, SidebarAction, keybind_action_for_name},
};

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
            let id = command
                .action()
                .split_once(':')
                .map_or(command.action(), |(id, _)| id);
            command.palette_action().is_some()
                && self
                    .describe(id)
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
            let action = command.action();
            let id = action.split_once(':').map_or(action, |(id, _)| id);
            let (title, description) = descriptor_metadata(id, command);
            let descriptor = CommandDescriptor {
                id: id.to_owned(),
                title: title.to_owned(),
                description: description.to_owned(),
                mutation: mutation_for(id),
                arguments: schema_for(id),
                target: target_for(id),
                palette: command.palette_action().is_some(),
            };
            commands
                .entry(id.to_owned())
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

fn descriptor_metadata(id: &str, command: Command) -> (&str, &str) {
    match id {
        "change_appearance" => (
            "Change Appearance",
            "Use the system, light, or dark appearance.",
        ),
        "move_tab" => (
            "Move Tab",
            "Move the selected tab by the signed position delta.",
        ),
        "navigate_search" => (
            "Navigate Search",
            "Move to the next or previous terminal search match.",
        ),
        _ => (command.title(), command.description()),
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

fn schema_for(id: &str) -> CompactSchema {
    let argument = match id {
        "select_tab" | "select_session" | "select_space" => {
            Some(bounded_integer("index", 1, i64::from(u32::MAX)))
        }
        "move_tab" | "move_session" => Some(bounded_integer(
            "delta",
            i64::from(i32::MIN),
            i64::from(i32::MAX),
        )),
        "scroll_page_lines" => Some(bounded_integer(
            "delta",
            i64::from(i16::MIN),
            i64::from(i16::MAX),
        )),
        "increase_font_size" | "decrease_font_size" | "set_font_size" => {
            Some(argument("size", ValueType::Number))
        }
        "select_pane" => Some(choice("direction", true, &["left", "right", "up", "down"])),
        "change_appearance" => Some(choice("appearance", true, &["system", "light", "dark"])),
        "navigate_search" => Some(choice("direction", true, &["next", "previous"])),
        "copy_to_clipboard" => Some(choice("format", false, &["plain", "vt", "html", "mixed"])),
        "csi" | "esc" | "text" | "search" => Some(argument("value", ValueType::String)),
        _ => None,
    };
    CompactSchema {
        arguments: argument.into_iter().collect(),
    }
}

fn bounded_integer(name: &str, minimum: i64, maximum: i64) -> ArgumentSchema {
    ArgumentSchema {
        minimum: Some(minimum),
        maximum: Some(maximum),
        ..argument(name, ValueType::Integer)
    }
}

fn choice(name: &str, required: bool, choices: &[&str]) -> ArgumentSchema {
    ArgumentSchema {
        name: name.to_owned(),
        value_type: ValueType::String,
        required,
        choices: choices.iter().map(|choice| (*choice).to_owned()).collect(),
        minimum: None,
        maximum: None,
    }
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

fn mutation_for(id: &str) -> MutationClass {
    const DESTRUCTIVE: &[&str] = &[
        "close_space",
        "close_surface",
        "close_window",
        "ditch_session",
        "kill_pane",
        "quit",
    ];
    const READ_ONLY: &[&str] = &["show_keybinds", "terminal.read"];
    if DESTRUCTIVE.contains(&id) {
        MutationClass::Destructive
    } else if READ_ONLY.contains(&id) {
        MutationClass::Read
    } else {
        MutationClass::Write
    }
}

fn target_for(id: &str) -> Option<ResourceKind> {
    match id {
        "quit" => Some(ResourceKind::Instance),
        "close_window"
        | "toggle_fullscreen"
        | "toggle_sidebar_focus"
        | "toggle_sidebar_visibility"
        | "open_settings"
        | "change_appearance"
        | "switch_theme"
        | "reload_config"
        | "create_space"
        | "next_space"
        | "previous_space"
        | "select_space"
        | "show_keybinds"
        | "command_palette"
        | "increase_font_size"
        | "decrease_font_size"
        | "reset_font_size"
        | "set_font_size" => Some(ResourceKind::ApplicationWindow),
        "new_window" | "new_mux_session" | "session_picker" | "next_session"
        | "previous_session" | "last_session" | "select_session" | "close_space" | "edit_space" => {
            Some(ResourceKind::Binding)
        }
        "new_tab" | "rename_session" | "ditch_session" | "move_session" | "next_tab"
        | "previous_tab" | "last_tab" | "select_tab" => Some(ResourceKind::Session),
        "move_tab" | "rename_tab" | "select_pane" | "next_pane" | "previous_pane" => {
            Some(ResourceKind::MuxWindow)
        }
        "split_right" | "split_down" | "kill_pane" | "close_surface" | "toggle_pane_zoom" => {
            Some(ResourceKind::Pane)
        }
        "scroll_to_top"
        | "scroll_to_bottom"
        | "scroll_page_up"
        | "scroll_page_down"
        | "scroll_page_lines"
        | "start_search"
        | "search"
        | "search_selection"
        | "navigate_search"
        | "end_search"
        | "copy_to_clipboard"
        | "copy_mode"
        | "paste_from_clipboard"
        | "csi"
        | "esc"
        | "text"
        | "terminal.read"
        | "terminal.write" => Some(ResourceKind::Terminal),
        _ => None,
    }
}
