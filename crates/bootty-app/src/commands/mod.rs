use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, LazyLock, RwLock},
};

pub use crate::mux::controller::CommandCancellation;
use serde::Deserialize;

use crate::{
    action_catalog::Command,
    app_actions::{KeybindAction, SidebarAction, keybind_action_for_name},
};

mod channel;

pub use channel::{
    AppCommandReceiver, AppCommandRequest, AppCommandSendError, AppCommandSender,
    BoundAppCommandSender, CommandCompletionContext, app_command_channel,
    app_command_channel_with_repaint,
};
mod model;

pub use model::{
    ArgumentSchema, BackendAvailability, COMMAND_OUTCOME_BYTE_LIMIT, COMMAND_RESULT_TOO_LARGE_CODE,
    Caller, CommandAvailability, CommandDescriptor, CommandInvocation, CommandOrigin,
    CommandOutcome, CommandPaletteMetadata, CommandResultSchema, CommandResultType, CommandTarget,
    CommandTargetKind, CommandWarning, CompactSchema, Confirmation, CoreCommandExecutor,
    MutationClass, MuxCommandSpec, ResourceKind, ValueType, bounded_command_outcome,
};
mod builtins;
mod validation;

use builtins::{
    canonical_action_id, compatibility_aliases, core_descriptor, explicit_core_commands,
    mutation_for,
};
use validation::{
    action_with_arguments, argument, invalid_pane_resize_argument, invalid_pane_select_argument,
    legacy_schema_for, legacy_target_for, normalize_arguments, parse_pane_resize_argument,
    parse_pane_select_direction, validate_arguments,
};

#[derive(Clone, Debug)]
struct ExtensionRegisteredCommand {
    descriptor: CommandDescriptor,
    extension_id: String,
    generation: u64,
}

#[derive(Clone, Debug, Default)]
struct ExtensionRegistryState {
    commands: BTreeMap<String, ExtensionRegisteredCommand>,
    aliases: BTreeMap<String, String>,
}

/// Mutable extension overlay owned by one app/control instance. It is
/// intentionally not global: two Bootty instances must not see one another's
/// commands or generation lifetimes.
#[derive(Clone, Debug, Default)]
pub struct ExtensionCommandRegistry {
    state: Arc<RwLock<ExtensionRegistryState>>,
}

impl ExtensionCommandRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedCommandInvocation {
    pub descriptor: CommandDescriptor,
    pub executor: CoreCommandExecutor,
    /// Kept as supplied so existing action-string dispatch and target fallback
    /// behavior remain intact even when the descriptor resolved through an alias.
    pub invocation: CommandInvocation,
}

#[derive(Clone, Debug)]
struct RegisteredCommand {
    descriptor: CommandDescriptor,
    executor: CommandExecutorResolver,
}

#[derive(Clone, Debug)]
struct RegisteredAlias {
    canonical: String,
    executor: Option<CommandExecutorResolver>,
    arguments: Option<CompactSchema>,
    target: Option<Option<ResourceKind>>,
}

#[derive(Clone, Debug)]
enum CommandExecutorResolver {
    Keybind(String),
    SplitPane,
    PaneSelect,
    PaneLast,
    PaneResize,
    Sidebar(SidebarAction),
    ReadTerminal,
    WriteTerminal,
    SessionSelect,
    SessionCreate,
    DirectoryResolve,
    DirectoryUsageList,
    WorktreeList,
    WorktreeGet,
    WorktreeCreate,
    WorktreeRemove,
    DirectControl,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
enum PaneResizeArgument {
    Directional {
        direction: String,
        cells: u16,
    },
    Absolute {
        columns: Option<u16>,
        rows: Option<u16>,
    },
}

#[derive(Clone, Debug)]
pub struct CommandRegistry {
    /// Canonical descriptors only. Keeping aliases separately makes `list`
    /// truthful while still allowing every established action spelling through
    /// the exact same dispatch seam.
    commands: BTreeMap<String, RegisteredCommand>,
    aliases: BTreeMap<String, RegisteredAlias>,
    parameterized_aliases: BTreeMap<String, BTreeMap<Vec<String>, String>>,
    extensions: Option<ExtensionCommandRegistry>,
}

impl CommandRegistry {
    pub fn core() -> &'static Self {
        static REGISTRY: LazyLock<CommandRegistry> =
            LazyLock::new(CommandRegistry::from_core_commands);
        &REGISTRY
    }
    /// Returns a registry view sharing the immutable core descriptors while
    /// reading/writing the supplied instance-local extension overlay.
    #[must_use]
    pub fn with_extension_registry(&self, extensions: ExtensionCommandRegistry) -> Self {
        Self {
            commands: self.commands.clone(),
            aliases: self.aliases.clone(),
            parameterized_aliases: self.parameterized_aliases.clone(),
            extensions: Some(extensions),
        }
    }

    /// Returns canonical core and live extension descriptors. Values are
    /// cloned so a reload can atomically replace its generation while a
    /// caller is enumerating the snapshot.
    pub fn list(&self) -> impl Iterator<Item = CommandDescriptor> {
        let extensions = self
            .extensions
            .as_ref()
            .and_then(|overlay| overlay.state.read().ok())
            .map(|registry| {
                registry
                    .commands
                    .values()
                    .map(|command| command.descriptor.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.commands
            .values()
            .map(|command| command.descriptor.clone())
            .chain(extensions)
    }

    /// Returns the effective descriptor for the supplied spelling.
    ///
    /// The returned `id` is always canonical. Source aliases can nevertheless
    /// retain their established positional schema and target semantics, so a
    /// caller that will invoke an alias must parse against this result rather
    /// than the canonical descriptor alone.
    pub fn describe(&self, id: &str) -> Option<CommandDescriptor> {
        if let Some(overlay) = &self.extensions
            && let Ok(registry) = overlay.state.read()
        {
            if let Some(command) = registry.commands.get(id) {
                return Some(command.descriptor.clone());
            }
            if let Some(canonical) = registry.aliases.get(id)
                && let Some(command) = registry.commands.get(canonical)
            {
                return Some(command.descriptor.clone());
            }
        }
        if let Some(command) = self.commands.get(id) {
            return Some(command.descriptor.clone());
        }
        let alias = self.aliases.get(id)?;
        let command = self.commands.get(&alias.canonical)?;
        Some(effective_alias_descriptor(command, alias))
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
    /// Registers one descriptor in the same live registry used by the core dispatcher.
    /// Core command IDs and aliases remain reserved.
    pub fn register_extension_command(
        &self,
        mut descriptor: CommandDescriptor,
        extension_id: impl Into<String>,
        generation: u64,
    ) -> Result<(), CommandOutcome> {
        let extension_id = extension_id.into();
        if !valid_extension_command_name(&descriptor.id) {
            return Err(CommandOutcome::Failed {
                code: "invalid_extension_command".to_owned(),
                message: format!("extension command name {} is invalid", descriptor.id),
            });
        }
        if extension_id.is_empty() || generation == 0 {
            return Err(CommandOutcome::Failed {
                code: "invalid_extension_generation".to_owned(),
                message: "extension id and generation are required".to_owned(),
            });
        }
        descriptor.origin = Some(CommandOrigin::Extension {
            extension_id: extension_id.clone(),
            generation,
        });
        let Some(overlay) = &self.extensions else {
            return Err(CommandOutcome::Failed {
                code: "extension_registry_unbound".to_owned(),
                message: "extension commands require an instance registry".to_owned(),
            });
        };
        let mut registry = overlay.state.write().map_err(|_| CommandOutcome::Failed {
            code: "registry_poisoned".to_owned(),
            message: "extension command registry is unavailable".to_owned(),
        })?;
        if self.commands.contains_key(&descriptor.id)
            || self.aliases.contains_key(&descriptor.id)
            || registry.commands.contains_key(&descriptor.id)
            || registry.aliases.contains_key(&descriptor.id)
        {
            return Err(CommandOutcome::Failed {
                code: "command_collision".to_owned(),
                message: format!("command {} is already registered", descriptor.id),
            });
        }
        if descriptor.aliases.iter().any(|alias| {
            !valid_extension_command_name(alias)
                || self.commands.contains_key(alias)
                || self.aliases.contains_key(alias)
                || registry.commands.contains_key(alias)
                || registry.aliases.contains_key(alias)
        }) {
            return Err(CommandOutcome::Failed {
                code: "command_collision".to_owned(),
                message: format!("command alias collides for {}", descriptor.id),
            });
        }
        let id = descriptor.id.clone();
        for alias in &descriptor.aliases {
            registry.aliases.insert(alias.clone(), id.clone());
        }
        registry.commands.insert(
            id,
            ExtensionRegisteredCommand {
                descriptor,
                extension_id,
                generation,
            },
        );
        Ok(())
    }

    /// Removes only the exact generation's commands and aliases. This makes a
    /// stale reload unable to unregister a replacement generation.
    pub fn unregister_extension_commands(&self, extension_id: &str, generation: u64) -> usize {
        let Some(overlay) = &self.extensions else {
            return 0;
        };
        let Ok(mut registry) = overlay.state.write() else {
            return 0;
        };
        let ids = registry
            .commands
            .iter()
            .filter(|(_, command)| {
                command.extension_id == extension_id && command.generation == generation
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in &ids {
            registry.commands.remove(id);
        }
        registry
            .aliases
            .retain(|_, canonical| !ids.contains(canonical));
        ids.len()
    }

    pub fn extension_commands(&self) -> Vec<CommandDescriptor> {
        self.extensions
            .as_ref()
            .and_then(|overlay| overlay.state.read().ok())
            .map(|registry| {
                registry
                    .commands
                    .values()
                    .map(|command| command.descriptor.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn resolve(
        &self,
        mut invocation: CommandInvocation,
    ) -> Result<ResolvedCommandInvocation, CommandOutcome> {
        if let Some((descriptor, extension_id, generation)) =
            extension_command_for(self.extensions.as_ref(), &invocation.command)
        {
            normalize_arguments(&descriptor, &mut invocation.arguments)?;
            validate_arguments(&descriptor, &invocation.arguments)?;
            return Ok(ResolvedCommandInvocation {
                descriptor: descriptor.clone(),
                executor: CoreCommandExecutor::Extension {
                    command_id: descriptor.id,
                    extension_id,
                    generation,
                },
                invocation,
            });
        }

        let (descriptor, resolver) = if let Some(registered) =
            self.commands.get(&invocation.command)
        {
            (registered.descriptor.clone(), registered.executor.clone())
        } else if let Some(alias) = self.aliases.get(&invocation.command) {
            let canonical = self
                .parameterized_aliases
                .get(&invocation.command)
                .and_then(|aliases| aliases.get(&invocation.arguments))
                .unwrap_or(&alias.canonical);
            let Some(registered) = self.commands.get(canonical) else {
                return Err(CommandOutcome::Failed {
                    code: "invalid_catalog".to_owned(),
                    message: format!("alias {} has no canonical descriptor", invocation.command),
                });
            };
            (
                effective_alias_descriptor(registered, alias),
                alias
                    .executor
                    .clone()
                    .unwrap_or_else(|| registered.executor.clone()),
            )
        } else {
            return Err(CommandOutcome::Failed {
                code: "unknown_command".to_owned(),
                message: format!("unknown command {}", invocation.command),
            });
        };

        normalize_arguments(&descriptor, &mut invocation.arguments)?;
        validate_arguments(&descriptor, &invocation.arguments)?;
        let executor = resolve_executor(&resolver, &invocation)?;

        // The descriptor stays canonical while the invocation retains its
        // source name. AppState still needs the latter for legacy target
        // fallback such as creating a first tab in an empty session.
        Ok(ResolvedCommandInvocation {
            descriptor,
            executor,
            invocation,
        })
    }

    fn from_core_commands() -> Self {
        let mut commands = BTreeMap::new();
        let mut aliases = BTreeMap::new();

        for command in Command::all() {
            let action = command.action();
            if action == "new_window" {
                continue;
            }
            let base = action.split_once(':').map_or(action, |(base, _)| base);
            let encoded_canonical = action.contains(':') && canonical_action_id(action).is_some();
            let canonical = canonical_action_id(action)
                .or_else(|| canonical_action_id(base))
                .unwrap_or(base);
            let canonical_arguments = if encoded_canonical {
                CompactSchema::default()
            } else {
                legacy_schema_for(base)
            };
            let canonical_target = if canonical.starts_with("space.") {
                Some(ResourceKind::Space)
            } else {
                legacy_target_for(base)
            };
            let canonical_executor = CommandExecutorResolver::Keybind(
                if encoded_canonical { action } else { base }.to_owned(),
            );
            commands
                .entry(canonical.to_owned())
                .or_insert_with(|| RegisteredCommand {
                    descriptor: core_descriptor(
                        canonical,
                        command.title(),
                        command.description(),
                        mutation_for(canonical),
                        canonical_arguments,
                        canonical_target,
                        command.palette_action().is_some(),
                    ),
                    executor: canonical_executor,
                });
            aliases.insert(
                action.to_owned(),
                RegisteredAlias {
                    canonical: canonical.to_owned(),
                    executor: Some(CommandExecutorResolver::Keybind(base.to_owned())),
                    arguments: Some(legacy_schema_for(base)),
                    target: Some(legacy_target_for(base)),
                },
            );
            aliases
                .entry(base.to_owned())
                .or_insert_with(|| RegisteredAlias {
                    canonical: canonical.to_owned(),
                    executor: Some(CommandExecutorResolver::Keybind(base.to_owned())),
                    arguments: Some(legacy_schema_for(base)),
                    target: Some(legacy_target_for(base)),
                });
        }

        for action in SidebarAction::ALL {
            let canonical = match action {
                SidebarAction::Ignore => "input.ignore",
                SidebarAction::PreviousSession => "session.previous",
                SidebarAction::NextSession => "session.next",
                SidebarAction::ActivateSession => "session.select",
                SidebarAction::FocusTerminal => "pane.focus",
            };
            aliases.insert(
                action.command_id().to_owned(),
                RegisteredAlias {
                    canonical: canonical.to_owned(),
                    executor: Some(CommandExecutorResolver::Sidebar(action)),
                    arguments: Some(CompactSchema::default()),
                    target: Some(
                        (action != SidebarAction::FocusTerminal)
                            .then_some(ResourceKind::ApplicationWindow),
                    ),
                },
            );
        }

        for (mut descriptor, executor) in explicit_core_commands() {
            if let Some(action) = commands.get(&descriptor.id) {
                descriptor.title.clone_from(&action.descriptor.title);
                descriptor
                    .description
                    .clone_from(&action.descriptor.description);
                descriptor.palette = action.descriptor.palette;
            }
            for alias in &descriptor.aliases {
                aliases.insert(
                    alias.clone(),
                    RegisteredAlias {
                        canonical: descriptor.id.clone(),
                        executor: None,
                        arguments: Some(descriptor.arguments.clone()),
                        target: Some(descriptor.target),
                    },
                );
            }
            commands.insert(
                descriptor.id.clone(),
                RegisteredCommand {
                    descriptor,
                    executor,
                },
            );
        }
        for &(alias, canonical) in compatibility_aliases() {
            let command = commands
                .get(canonical)
                .unwrap_or_else(|| panic!("compatibility alias {alias} has no live command"));
            aliases
                .entry(alias.to_owned())
                .or_insert_with(|| RegisteredAlias {
                    canonical: canonical.to_owned(),
                    executor: None,
                    arguments: Some(command.descriptor.arguments.clone()),
                    target: Some(command.descriptor.target),
                });
        }
        aliases.insert(
            "terminal.write".to_owned(),
            RegisteredAlias {
                canonical: "terminal.send_text".to_owned(),
                executor: Some(CommandExecutorResolver::WriteTerminal),
                arguments: Some(CompactSchema {
                    arguments: vec![argument("text", ValueType::String)],
                }),
                target: Some(Some(ResourceKind::Terminal)),
            },
        );

        let mut parameterized_aliases = BTreeMap::new();
        for (alias, registration) in &aliases {
            let Some((base, values)) = alias.split_once(':') else {
                continue;
            };
            let values = values.split(':').map(str::to_owned).collect::<Vec<_>>();
            parameterized_aliases
                .entry(base.to_owned())
                .or_insert_with(BTreeMap::new)
                .insert(values, registration.canonical.clone());
        }
        for (alias, registration) in &aliases {
            if alias != &registration.canonical {
                commands
                    .get_mut(&registration.canonical)
                    .expect("live alias must reference a live command")
                    .descriptor
                    .aliases
                    .push(alias.clone());
            }
        }
        for command in commands.values_mut() {
            command.descriptor.aliases.sort();
            command.descriptor.aliases.dedup();
        }

        Self {
            commands,
            aliases,
            parameterized_aliases,
            extensions: None,
        }
    }
}

fn valid_extension_command_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
}
pub(crate) fn is_direct_control_command(id: &str) -> bool {
    matches!(
        id,
        "system.ping"
            | "system.describe"
            | "instance.describe"
            | "command.list"
            | "command.describe"
            | "command.invoke"
            | "event.subscribe"
            | "event.snapshot"
            | "event.rebase"
            | "event.unsubscribe"
            | "task.status"
            | "task.cancel"
    )
}

fn extension_command_for(
    overlay: Option<&ExtensionCommandRegistry>,
    id: &str,
) -> Option<(CommandDescriptor, String, u64)> {
    let registry = overlay?.state.read().ok()?;
    let canonical = registry.aliases.get(id).map_or(id, String::as_str);
    registry.commands.get(canonical).map(|command| {
        (
            command.descriptor.clone(),
            command.extension_id.clone(),
            command.generation,
        )
    })
}

fn effective_alias_descriptor(
    command: &RegisteredCommand,
    alias: &RegisteredAlias,
) -> CommandDescriptor {
    let mut descriptor = command.descriptor.clone();
    if let Some(arguments) = &alias.arguments {
        descriptor.arguments = arguments.clone();
    }
    if let Some(target) = alias.target {
        descriptor.target = target;
    }
    descriptor
}

fn resolve_executor(
    resolver: &CommandExecutorResolver,
    invocation: &CommandInvocation,
) -> Result<CoreCommandExecutor, CommandOutcome> {
    match resolver {
        CommandExecutorResolver::Keybind(action) => {
            let action = action_with_arguments(action, &invocation.arguments);
            keybind_action_for_name(&action)
                .map(CoreCommandExecutor::Keybind)
                .ok_or_else(|| CommandOutcome::Unsupported {
                    message: format!("command {} has no app executor", invocation.command),
                })
        }
        CommandExecutorResolver::SplitPane => {
            let action = match invocation.arguments.first().map(String::as_str) {
                Some("right") => "split_right",
                Some("down") => "split_down",
                _ => {
                    return Err(CommandOutcome::Failed {
                        code: "invalid_arguments".to_owned(),
                        message: "pane.split requires direction right or down".to_owned(),
                    });
                }
            };
            keybind_action_for_name(action)
                .map(CoreCommandExecutor::Keybind)
                .ok_or_else(|| CommandOutcome::Unsupported {
                    message: "pane.split has no app executor".to_owned(),
                })
        }
        CommandExecutorResolver::PaneSelect => {
            let direction = invocation
                .arguments
                .first()
                .map(String::as_str)
                .ok_or_else(invalid_pane_select_argument)
                .and_then(parse_pane_select_direction)?;
            Ok(CoreCommandExecutor::Mux(MuxCommandSpec::SelectPane {
                direction,
            }))
        }
        CommandExecutorResolver::PaneLast => {
            Ok(CoreCommandExecutor::Mux(MuxCommandSpec::SelectLastPane))
        }
        CommandExecutorResolver::PaneResize => {
            let adjustment = invocation
                .arguments
                .first()
                .ok_or_else(invalid_pane_resize_argument)
                .and_then(|argument| parse_pane_resize_argument(argument))?;
            Ok(CoreCommandExecutor::Mux(MuxCommandSpec::ResizePane {
                adjustment,
            }))
        }
        CommandExecutorResolver::Sidebar(action) => Ok(CoreCommandExecutor::Sidebar(*action)),
        CommandExecutorResolver::ReadTerminal => Ok(CoreCommandExecutor::ReadTerminal),
        CommandExecutorResolver::WriteTerminal => Ok(CoreCommandExecutor::Keybind(
            KeybindAction::Write(invocation.arguments[0].as_bytes().to_vec()),
        )),
        CommandExecutorResolver::SessionSelect => Ok(CoreCommandExecutor::SessionSelect {
            selector: invocation.arguments[0].clone(),
        }),
        CommandExecutorResolver::SessionCreate => serde_json::from_str(&invocation.arguments[0])
            .map(CoreCommandExecutor::SessionCreate)
            .map_err(|error| CommandOutcome::Failed {
                code: "invalid_arguments".to_owned(),
                message: format!("invalid session.create launch descriptor: {error}"),
            }),
        CommandExecutorResolver::DirectoryResolve => Ok(CoreCommandExecutor::DirectoryResolve {
            path: invocation.arguments[0].clone(),
        }),
        CommandExecutorResolver::DirectoryUsageList => {
            Ok(CoreCommandExecutor::DirectoryUsageList {
                path: invocation.arguments[0].clone(),
            })
        }
        CommandExecutorResolver::WorktreeList => Ok(CoreCommandExecutor::WorktreeList {
            path: invocation.arguments[0].clone(),
        }),
        CommandExecutorResolver::WorktreeGet => Ok(CoreCommandExecutor::WorktreeGet {
            path: invocation.arguments[0].clone(),
        }),
        CommandExecutorResolver::WorktreeCreate => Ok(CoreCommandExecutor::WorktreeCreate {
            repository_path: invocation.arguments[0].clone(),
            branch: invocation.arguments[1].clone(),
            managed_by_bootty: invocation.arguments[2]
                .parse()
                .expect("validated managed_by_bootty boolean"),
        }),
        CommandExecutorResolver::WorktreeRemove => {
            let confirmation = invocation
                .arguments
                .get(2)
                .map(|value| {
                    serde_json::from_str(value).map_err(|error| CommandOutcome::Failed {
                        code: "invalid_arguments".to_owned(),
                        message: format!("invalid worktree.remove confirmation: {error}"),
                    })
                })
                .transpose()?;
            Ok(CoreCommandExecutor::WorktreeRemove {
                path: invocation.arguments[0].clone(),
                force: invocation.arguments[1]
                    .parse()
                    .expect("validated force boolean"),
                confirmation,
            })
        }
        CommandExecutorResolver::DirectControl => Err(CommandOutcome::Failed {
            code: "direct_control_only".to_owned(),
            message: format!(
                "command {} is available only through its direct control-plane RPC method",
                invocation.command
            ),
        }),
    }
}

pub fn core_command_ids() -> BTreeSet<String> {
    CommandRegistry::core()
        .list()
        .map(|descriptor| descriptor.id.clone())
        .collect()
}
