use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, LazyLock, Mutex, RwLock,
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    time::Instant,
};

pub use crate::mux::controller::CommandCancellation;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    action_catalog::Command,
    app_actions::{KeybindAction, SidebarAction, keybind_action_for_name},
    automation::{
        catalog::{
            BackendAvailability, CanonicalDescriptor, CatalogAvailability, CatalogMutation,
            CatalogOrigin, CatalogPaletteMetadata, CatalogResultSchema, CatalogTarget,
            CatalogValueType, SourceMappingKind, canonical_catalog,
        },
        directory::WorktreeRemovalConfirmation,
        launch::SessionLaunchDescriptor,
    },
    mux::{
        RepaintHandle,
        command::{MuxDirection, MuxPaneResize},
    },
};

mod decimal_u64 {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<u64, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Caller {
    CommandPalette,
    Keybinding,
    BuiltinKeybinding,
    Cli,
    Socket,
    Luau,
    Internal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationClass {
    Read,
    Write,
    Destructive,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    Null,
    Boolean,
    Integer,
    Number,
    String,
    Enum,
    Array,
    Object,
    ResourceRef,
    Json,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgumentSchema {
    pub name: String,
    pub value_type: ValueType,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default)]
    pub repeated: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactSchema {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<ArgumentSchema>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandDescriptor {
    pub id: String,
    pub title: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<CatalogOrigin>,
    pub mutation: MutationClass,
    pub arguments: CompactSchema,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_schema: Option<CatalogResultSchema>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<CatalogTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<BackendAvailability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<ResourceKind>,
    #[serde(default)]
    pub palette: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub palette_metadata: Option<CatalogPaletteMetadata>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Instance,
    ApplicationWindow,
    Binding,
    Space,
    Session,
    MuxWindow,
    Pane,
    Terminal,
    Client,
    Directory,
    Worktree,
    Task,
    Subscription,
    Surface,
    Extension,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandTarget {
    pub kind: ResourceKind,
    /// Host-issued scoped identifier. Callers must treat this value as opaque.
    pub handle: String,
    #[serde(with = "decimal_u64")]
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Confirmation {
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<CommandTarget>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandInvocation {
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<String>,
    pub caller: Caller,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<CommandTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<Confirmation>,
}

impl CommandInvocation {
    // ponytail: action-string arguments bridge existing keybindings; replace them with schema values
    // when the external command parser lands.
    pub fn from_action(action: &str, caller: Caller) -> Self {
        let (command, arguments) = action
            .split_once(':')
            .map_or((action, Vec::new()), |(command, arguments)| {
                (command, vec![arguments.to_owned()])
            });
        Self {
            command: command.to_owned(),
            arguments,
            caller,
            target: None,
            confirmation: None,
        }
    }

    pub fn from_catalog(command: Command, caller: Caller) -> Option<Self> {
        command
            .palette_action()
            .map(|action| Self::from_action(action, caller))
    }

    pub fn confirmation(&self) -> Confirmation {
        Confirmation {
            command: self.command.clone(),
            arguments: self.arguments.clone(),
            target: self.target.clone(),
        }
    }
}

pub const COMMAND_OUTCOME_BYTE_LIMIT: usize = 256 * 1024;
pub const COMMAND_RESULT_TOO_LARGE_CODE: &str = "result_too_large";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandWarning {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CommandOutcome {
    Success {
        #[serde(default)]
        value: Value,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        warnings: Vec<CommandWarning>,
    },
    Pending {
        #[serde(skip_serializing_if = "Option::is_none")]
        target: Option<CommandTarget>,
    },
    Unsupported {
        message: String,
    },
    Unavailable {
        message: String,
    },
    Denied {
        message: String,
    },
    StaleTarget {
        message: String,
    },
    Ambiguous {
        message: String,
        candidates: Vec<CommandTarget>,
    },
    ConfirmationRequired {
        confirmation: Box<Confirmation>,
    },
    Failed {
        code: String,
        message: String,
    },
}

impl CommandOutcome {
    pub fn success() -> Self {
        Self::Success {
            value: Value::Null,
            warnings: Vec::new(),
        }
    }

    pub fn success_with_warning(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Success {
            value: Value::Null,
            warnings: vec![CommandWarning {
                code: code.into(),
                message: message.into(),
            }],
        }
    }

    pub fn pending(target: Option<CommandTarget>) -> Self {
        Self::Pending { target }
    }

    pub fn completion_indeterminate() -> Self {
        Self::Failed {
            code: "completion_indeterminate".to_owned(),
            message: "command started before its deadline; completion is being reconciled"
                .to_owned(),
        }
    }
}

pub fn bounded_command_outcome(outcome: CommandOutcome) -> CommandOutcome {
    let Ok(bytes) = serde_json::to_vec(&outcome) else {
        return CommandOutcome::Failed {
            code: COMMAND_RESULT_TOO_LARGE_CODE.to_owned(),
            message: "command result could not be serialized within the result limit".to_owned(),
        };
    };
    if bytes.len() <= COMMAND_OUTCOME_BYTE_LIMIT {
        return outcome;
    }
    CommandOutcome::Failed {
        code: COMMAND_RESULT_TOO_LARGE_CODE.to_owned(),
        message: format!(
            "serialized command result is {} bytes; limit is {} bytes",
            bytes.len(),
            COMMAND_OUTCOME_BYTE_LIMIT
        ),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum MuxCommandSpec {
    SelectPane { direction: MuxDirection },
    SelectLastPane,
    ResizePane { adjustment: MuxPaneResize },
}

#[derive(Clone, Debug, PartialEq)]
pub enum CoreCommandExecutor {
    Mux(MuxCommandSpec),
    Keybind(KeybindAction),
    Sidebar(SidebarAction),
    ReadTerminal,
    SessionSelect {
        selector: String,
    },
    SessionCreate(SessionLaunchDescriptor),
    DirectoryResolve {
        path: String,
    },
    DirectoryUsageList {
        path: String,
    },
    WorktreeList {
        path: String,
    },
    WorktreeGet {
        path: String,
    },
    WorktreeCreate {
        repository_path: String,
        branch: String,
        managed_by_bootty: bool,
    },
    WorktreeRemove {
        path: String,
        force: bool,
        confirmation: Option<WorktreeRemovalConfirmation>,
    },
    /// A command owned by a live Luau extension generation. The handler is
    /// resolved by `ExtensionRuntime` after the common registry has validated
    /// its descriptor and arguments.
    Extension {
        command_id: String,
        extension_id: String,
        generation: u64,
    },
}

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
    Catalog(CatalogAvailability),
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
                let ids = registry.commands.keys().cloned().collect::<BTreeSet<_>>();
                let commands = registry
                    .commands
                    .values()
                    .map(|command| command.descriptor.clone())
                    .collect::<Vec<_>>();
                (ids, commands)
            })
            .unwrap_or_default();
        let (extension_ids, extension_commands) = extensions;
        let core = self
            .commands
            .values()
            .filter(|command| !extension_ids.contains(&command.descriptor.id))
            .map(|command| command.descriptor.clone())
            .collect::<Vec<_>>();
        core.into_iter().chain(extension_commands)
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
    /// Registers one descriptor in the same live registry used by the core
    /// dispatcher. An extension may replace any catalog-only unavailable
    /// placeholder; every runnable core spelling remains reserved.
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
        descriptor.origin = Some(CatalogOrigin::Extension {
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
        if (self.commands.contains_key(&descriptor.id) || self.aliases.contains_key(&descriptor.id))
            && !core_placeholder_matches(
                &self.commands,
                &self.aliases,
                &descriptor.id,
                &descriptor.id,
            )
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
                || ((self.commands.contains_key(alias) || self.aliases.contains_key(alias))
                    && !core_placeholder_matches(
                        &self.commands,
                        &self.aliases,
                        alias,
                        &descriptor.id,
                    ))
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

        if matches!(
            &resolver,
            CommandExecutorResolver::Catalog(CatalogAvailability::Unavailable)
        ) {
            return Err(unavailable_command_outcome(&invocation.command));
        }
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
        let catalog = canonical_catalog();
        let mut commands = BTreeMap::new();
        for descriptor in catalog.descriptors() {
            let registry_descriptor = registry_descriptor_for(descriptor);
            let executor = canonical_executor(descriptor);
            if matches!(
                descriptor.availability.core,
                CatalogAvailability::Available | CatalogAvailability::Conditional
            ) && matches!(&executor, CommandExecutorResolver::Catalog(_))
            {
                panic!(
                    "catalog marks {} available without a core executor",
                    descriptor.id
                );
            }
            commands.insert(
                registry_descriptor.id.clone(),
                RegisteredCommand {
                    executor,
                    descriptor: registry_descriptor,
                },
            );
        }

        let mut aliases = BTreeMap::new();
        for descriptor in catalog.descriptors() {
            for alias in &descriptor.aliases {
                aliases.insert(
                    alias.clone(),
                    RegisteredAlias {
                        canonical: descriptor.id.clone(),
                        executor: None,
                        arguments: None,
                        target: None,
                    },
                );
            }
        }

        // Existing action strings remain the runnable UI/keybinding surface,
        // except source-manifested local-only actions. Their descriptor and
        // eventual outcome are canonical, but their executor, positional
        // schema, and target semantics remain unchanged.
        for command in Command::all() {
            let action = command.action();
            let id = action.split_once(':').map_or(action, |(id, _)| id);
            let Some(alias) = aliases.get_mut(id) else {
                if catalog
                    .source_mapping("bootty_actions", id)
                    .is_some_and(|mapping| mapping.kind == SourceMappingKind::Unsupported)
                {
                    continue;
                }
                panic!("checked-in catalog is missing Bootty action alias {id}");
            };
            alias.executor = Some(CommandExecutorResolver::Keybind(id.to_owned()));
            alias.arguments = Some(legacy_schema_for(id));
            alias.target = Some(legacy_target_for(id));
        }
        for action in SidebarAction::ALL {
            let alias = aliases
                .get_mut(action.command_id())
                .unwrap_or_else(|| panic!("checked-in catalog is missing sidebar alias"));
            alias.executor = Some(CommandExecutorResolver::Sidebar(action));
            alias.arguments = Some(CompactSchema::default());
            if action != SidebarAction::FocusTerminal {
                alias.target = Some(Some(ResourceKind::ApplicationWindow));
            }
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
            if values.is_empty() || !aliases.contains_key(base) {
                panic!("catalog parameterized alias {alias} has no base alias");
            }
            let values = values.split(':').map(str::to_owned).collect::<Vec<_>>();
            if values.iter().any(String::is_empty)
                || parameterized_aliases
                    .entry(base.to_owned())
                    .or_insert_with(BTreeMap::new)
                    .insert(values, registration.canonical.clone())
                    .is_some()
            {
                panic!("catalog parameterized alias {alias} is invalid or duplicated");
            }
        }
        for (alias, registration) in &aliases {
            let canonical = commands
                .get(&registration.canonical)
                .unwrap_or_else(|| panic!("catalog alias {alias} has no canonical descriptor"));
            if registration.executor.is_some()
                && !is_core_dispatchable(
                    canonical
                        .descriptor
                        .availability
                        .as_ref()
                        .expect("catalog descriptors always carry availability")
                        .core,
                )
            {
                panic!(
                    "catalog alias {alias} has an executor despite explicit core unavailability"
                );
            }
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
fn core_placeholder_matches(
    commands: &BTreeMap<String, RegisteredCommand>,
    aliases: &BTreeMap<String, RegisteredAlias>,
    spelling: &str,
    canonical: &str,
) -> bool {
    let command = commands.get(spelling).or_else(|| {
        aliases
            .get(spelling)
            .and_then(|alias| commands.get(&alias.canonical))
    });
    command.is_some_and(|command| {
        command.descriptor.id == canonical
            && matches!(
                command.descriptor.origin.as_ref(),
                Some(CatalogOrigin::Extension { .. })
            )
            && command
                .descriptor
                .availability
                .as_ref()
                .is_some_and(|availability| availability.core == CatalogAvailability::Unavailable)
    })
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

fn registry_descriptor_for(descriptor: &CanonicalDescriptor) -> CommandDescriptor {
    CommandDescriptor {
        id: descriptor.id.clone(),
        title: descriptor.title.clone(),
        description: descriptor.description.clone(),
        aliases: descriptor.aliases.clone(),
        origin: Some(descriptor.origin.clone()),
        mutation: mutation_from_catalog(descriptor.mutation),
        arguments: compact_schema_from_catalog(&descriptor.argument_schema),
        result_schema: Some(descriptor.result_schema.clone()),
        targets: descriptor.targets.clone(),
        availability: Some(descriptor.availability.clone()),
        target: descriptor
            .targets
            .first()
            .map(resource_kind_from_catalog_target),
        palette: descriptor.palette.visible,
        palette_metadata: Some(descriptor.palette.clone()),
    }
}

fn mutation_from_catalog(mutation: CatalogMutation) -> MutationClass {
    match mutation {
        CatalogMutation::Read => MutationClass::Read,
        CatalogMutation::Write => MutationClass::Write,
        CatalogMutation::Destructive => MutationClass::Destructive,
    }
}

fn compact_schema_from_catalog(
    arguments: &[crate::automation::catalog::CatalogArgumentSchema],
) -> CompactSchema {
    CompactSchema {
        arguments: arguments
            .iter()
            .map(|argument| ArgumentSchema {
                name: argument.name.clone(),
                value_type: value_type_from_catalog(argument.value_type),
                required: argument.required,
                choices: argument.choices.clone(),
                minimum: argument.minimum,
                maximum: argument.maximum,
                default: argument.default.clone(),
                repeated: argument.repeated,
            })
            .collect(),
    }
}

fn value_type_from_catalog(value_type: CatalogValueType) -> ValueType {
    match value_type {
        CatalogValueType::Null => ValueType::Null,
        CatalogValueType::Boolean => ValueType::Boolean,
        CatalogValueType::Integer => ValueType::Integer,
        CatalogValueType::Number => ValueType::Number,
        CatalogValueType::String => ValueType::String,
        CatalogValueType::Enum => ValueType::Enum,
        CatalogValueType::Array => ValueType::Array,
        CatalogValueType::Object => ValueType::Object,
        CatalogValueType::ResourceRef => ValueType::ResourceRef,
        CatalogValueType::Json => ValueType::Json,
    }
}

fn resource_kind_from_catalog_target(target: &CatalogTarget) -> ResourceKind {
    match target {
        CatalogTarget::Instance => ResourceKind::Instance,
        CatalogTarget::ApplicationWindow => ResourceKind::ApplicationWindow,
        CatalogTarget::Binding => ResourceKind::Binding,
        CatalogTarget::Space => ResourceKind::Space,
        CatalogTarget::Session => ResourceKind::Session,
        CatalogTarget::Window => ResourceKind::MuxWindow,
        CatalogTarget::Pane => ResourceKind::Pane,
        CatalogTarget::Terminal => ResourceKind::Terminal,
        CatalogTarget::Client => ResourceKind::Client,
        CatalogTarget::Directory => ResourceKind::Directory,
        CatalogTarget::Worktree => ResourceKind::Worktree,
        CatalogTarget::Task => ResourceKind::Task,
        CatalogTarget::Subscription => ResourceKind::Subscription,
        CatalogTarget::Surface => ResourceKind::Surface,
        CatalogTarget::Extension => ResourceKind::Extension,
    }
}

fn is_core_dispatchable(availability: CatalogAvailability) -> bool {
    matches!(
        availability,
        CatalogAvailability::Available | CatalogAvailability::Conditional
    )
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

fn canonical_executor(descriptor: &CanonicalDescriptor) -> CommandExecutorResolver {
    if !is_core_dispatchable(descriptor.availability.core) {
        return CommandExecutorResolver::Catalog(descriptor.availability.core);
    }
    if is_direct_control_command(&descriptor.id) {
        return CommandExecutorResolver::DirectControl;
    }
    if descriptor.id == "pane.focus" {
        return CommandExecutorResolver::Sidebar(SidebarAction::FocusTerminal);
    }

    let keybind = match descriptor.id.as_str() {
        "app.fullscreen.toggle" => Some("toggle_fullscreen"),
        "app.quit" => Some("quit"),
        "app.window.close" => Some("close_window"),
        "appearance.set" => Some("change_appearance"),
        "clipboard.copy" => Some("copy_to_clipboard"),
        "clipboard.paste" => Some("paste_from_clipboard"),
        "config.reload" => Some("reload_config"),
        "font.decrease" => Some("decrease_font_size"),
        "font.increase" => Some("increase_font_size"),
        "font.reset" => Some("reset_font_size"),
        "font.set" => Some("set_font_size"),
        "input.ignore" => Some("ignore"),
        "pane.close" => Some("close_surface"),
        "pane.focus_direction" => Some("select_pane"),
        "pane.kill" => Some("kill_pane"),
        "pane.next" => Some("next_pane"),
        "pane.previous" => Some("previous_pane"),
        "pane.zoom" => Some("toggle_pane_zoom"),
        "session.ditch" => Some("ditch_session"),
        "session.last" => Some("last_session"),
        "session.move" => Some("move_session"),
        "session.next" => Some("next_session"),
        "session.picker" => Some("session_picker"),
        "session.previous" => Some("previous_session"),
        "session.rename" => Some("rename_session"),
        "space.close" => Some("close_space"),
        "space.create" => Some("create_space"),
        "space.edit" => Some("edit_space"),
        "space.next" => Some("next_space"),
        "space.previous" => Some("previous_space"),
        "space.select" => Some("select_space"),
        "terminal.copy_mode" => Some("copy_mode"),
        "terminal.scroll.bottom" => Some("scroll_to_bottom"),
        "terminal.scroll.lines" => Some("scroll_page_lines"),
        "terminal.scroll.page_down" => Some("scroll_page_down"),
        "terminal.scroll.page_up" => Some("scroll_page_up"),
        "terminal.scroll.top" => Some("scroll_to_top"),
        "terminal.search" => Some("search"),
        "terminal.search.close" => Some("end_search"),
        "terminal.search.next" => Some("navigate_search:next"),
        "terminal.search.previous" => Some("navigate_search:previous"),
        "terminal.search.selection" => Some("search_selection"),
        "terminal.search.start" => Some("start_search"),
        "terminal.send_csi" => Some("csi"),
        "terminal.send_esc" => Some("esc"),
        "terminal.send_text" => Some("text"),
        "ui.command_palette.open" => Some("command_palette"),
        "ui.keybindings.open" => Some("show_keybinds"),
        "ui.settings.open" => Some("open_settings"),
        "ui.sidebar.focus" => Some("toggle_sidebar_focus"),
        "ui.sidebar.toggle" => Some("toggle_sidebar_visibility"),
        "ui.theme_picker.open" => Some("switch_theme"),
        "window.create" => Some("new_tab"),
        "window.last" => Some("last_tab"),
        "window.move" => Some("move_tab"),
        "window.next" => Some("next_tab"),
        "window.previous" => Some("previous_tab"),
        "window.rename" => Some("rename_tab"),
        "window.select" => Some("select_tab"),
        _ => None,
    };
    if let Some(action) = keybind {
        return CommandExecutorResolver::Keybind(action.to_owned());
    }
    match descriptor.id.as_str() {
        "pane.split" => CommandExecutorResolver::SplitPane,
        "terminal.read" => CommandExecutorResolver::ReadTerminal,
        "pane.select" => CommandExecutorResolver::PaneSelect,
        "pane.last" => CommandExecutorResolver::PaneLast,
        "pane.resize" => CommandExecutorResolver::PaneResize,
        "session.select" => CommandExecutorResolver::SessionSelect,
        "session.create" => CommandExecutorResolver::SessionCreate,
        "directory.resolve" => CommandExecutorResolver::DirectoryResolve,
        "directory.usage.list" => CommandExecutorResolver::DirectoryUsageList,
        "worktree.list" => CommandExecutorResolver::WorktreeList,
        "worktree.get" => CommandExecutorResolver::WorktreeGet,
        "worktree.create" => CommandExecutorResolver::WorktreeCreate,
        "worktree.remove" => CommandExecutorResolver::WorktreeRemove,
        _ => CommandExecutorResolver::Catalog(descriptor.availability.core),
    }
}

fn unavailable_command_outcome(command: &str) -> CommandOutcome {
    CommandOutcome::Unavailable {
        message: format!("command {command} is currently unavailable"),
    }
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
        CommandExecutorResolver::Catalog(CatalogAvailability::Unavailable) => {
            Err(unavailable_command_outcome(&invocation.command))
        }
        CommandExecutorResolver::Catalog(
            CatalogAvailability::Available
            | CatalogAvailability::Conditional
            | CatalogAvailability::Unsupported,
        ) => Err(CommandOutcome::Unsupported {
            message: format!(
                "command {} is not implemented by this core dispatcher",
                invocation.command
            ),
        }),
    }
}

fn invalid_pane_select_argument() -> CommandOutcome {
    CommandOutcome::Failed {
        code: "invalid_arguments".to_owned(),
        message: "pane.select requires direction left, right, up, or down".to_owned(),
    }
}

fn parse_pane_select_direction(value: &str) -> Result<MuxDirection, CommandOutcome> {
    match value {
        "left" => Ok(MuxDirection::Left),
        "right" => Ok(MuxDirection::Right),
        "up" => Ok(MuxDirection::Up),
        "down" => Ok(MuxDirection::Down),
        _ => Err(invalid_pane_select_argument()),
    }
}

fn invalid_pane_resize_argument() -> CommandOutcome {
    CommandOutcome::Failed {
        code: "invalid_arguments".to_owned(),
        message: "pane.resize requires a positive directional or absolute adjustment".to_owned(),
    }
}

fn parse_pane_resize_argument(value: &str) -> Result<MuxPaneResize, CommandOutcome> {
    let argument = serde_json::from_str::<PaneResizeArgument>(value)
        .map_err(|_| invalid_pane_resize_argument())?;
    let adjustment = match argument {
        PaneResizeArgument::Directional { direction, cells } => {
            let direction = match direction.as_str() {
                "left" => MuxDirection::Left,
                "down" => MuxDirection::Down,
                "up" => MuxDirection::Up,
                "right" => MuxDirection::Right,
                _ => return Err(invalid_pane_resize_argument()),
            };
            MuxPaneResize::Directional { direction, cells }
        }
        PaneResizeArgument::Absolute { columns, rows } => MuxPaneResize::Absolute { columns, rows },
    };
    adjustment
        .is_valid()
        .then_some(adjustment)
        .ok_or_else(invalid_pane_resize_argument)
}

fn action_with_arguments(action: &str, arguments: &[String]) -> String {
    match arguments {
        [] => action.to_owned(),
        arguments => format!("{action}:{}", arguments.join(":")),
    }
}

fn normalize_arguments(
    descriptor: &CommandDescriptor,
    arguments: &mut Vec<String>,
) -> Result<(), CommandOutcome> {
    let schemas = &descriptor.arguments.arguments;
    if schemas
        .iter()
        .enumerate()
        .any(|(index, schema)| schema.repeated && index + 1 != schemas.len())
    {
        return Err(invalid_arguments(
            descriptor,
            "only the final argument may be repeated",
        ));
    }
    if !schemas.last().is_some_and(|schema| schema.repeated) && arguments.len() > schemas.len() {
        return Err(invalid_arguments(
            descriptor,
            &format!(
                "expects at most {} argument(s), got {}",
                schemas.len(),
                arguments.len()
            ),
        ));
    }
    for schema in schemas.iter().skip(arguments.len()) {
        if let Some(default) = &schema.default {
            arguments.push(default.clone());
        } else if schema.required {
            return Err(invalid_arguments(
                descriptor,
                &format!("is missing required {} argument", schema.name),
            ));
        }
    }
    Ok(())
}

fn validate_arguments(
    descriptor: &CommandDescriptor,
    arguments: &[String],
) -> Result<(), CommandOutcome> {
    let schemas = &descriptor.arguments.arguments;
    let repeated = schemas.last().is_some_and(|schema| schema.repeated);
    let required = schemas.iter().filter(|argument| argument.required).count();
    if arguments.len() < required || (!repeated && arguments.len() > schemas.len()) {
        return Err(invalid_arguments(
            descriptor,
            &format!(
                "expects {}{} argument(s), got {}",
                required,
                if repeated { " or more" } else { "" },
                arguments.len()
            ),
        ));
    }
    for (index, value) in arguments.iter().enumerate() {
        let schema = schemas
            .get(index)
            .or_else(|| repeated.then(|| schemas.last()).flatten())
            .expect("argument count was validated");
        let json = || serde_json::from_str::<Value>(value).ok();
        let valid_type = match schema.value_type {
            ValueType::Null => matches!(json(), Some(Value::Null)),
            ValueType::Boolean => value.parse::<bool>().is_ok(),
            ValueType::Integer => value.parse::<i64>().is_ok(),
            ValueType::Number => value.parse::<f32>().is_ok_and(f32::is_finite),
            ValueType::String => true,
            ValueType::Enum | ValueType::ResourceRef => !value.is_empty(),
            ValueType::Array => matches!(json(), Some(Value::Array(_))),
            ValueType::Object => matches!(json(), Some(Value::Object(_))),
            ValueType::Json => json().is_some(),
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
            return Err(invalid_arguments(
                descriptor,
                &format!("has an invalid {} argument", schema.name),
            ));
        }
    }
    Ok(())
}

fn invalid_arguments(descriptor: &CommandDescriptor, detail: &str) -> CommandOutcome {
    CommandOutcome::Failed {
        code: "invalid_arguments".to_owned(),
        message: format!("command {} {detail}", descriptor.id),
    }
}

fn legacy_schema_for(id: &str) -> CompactSchema {
    let argument = match id {
        "select_tab" | "select_session" | "select_space" => Some(ArgumentSchema {
            name: "index".to_owned(),
            value_type: ValueType::Integer,
            required: true,
            choices: Vec::new(),
            minimum: Some(1),
            maximum: Some(i64::from(u32::MAX)),
            default: None,
            repeated: false,
        }),
        "move_tab" | "move_session" => Some(ArgumentSchema {
            name: "delta".to_owned(),
            value_type: ValueType::Integer,
            required: true,
            choices: Vec::new(),
            minimum: Some(i64::from(i32::MIN)),
            maximum: Some(i64::from(i32::MAX)),
            default: None,
            repeated: false,
        }),
        "scroll_page_lines" => Some(ArgumentSchema {
            name: "delta".to_owned(),
            value_type: ValueType::Integer,
            required: true,
            choices: Vec::new(),
            minimum: Some(i64::from(i16::MIN)),
            maximum: Some(i64::from(i16::MAX)),
            default: None,
            repeated: false,
        }),
        "increase_font_size" | "decrease_font_size" | "set_font_size" => {
            Some(argument("size", ValueType::Number))
        }
        "select_pane" => Some(ArgumentSchema {
            name: "direction".to_owned(),
            value_type: ValueType::Enum,
            required: true,
            choices: ["left", "right", "up", "down"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            minimum: None,
            maximum: None,
            default: None,
            repeated: false,
        }),
        "change_appearance" => Some(ArgumentSchema {
            name: "appearance".to_owned(),
            value_type: ValueType::Enum,
            required: true,
            choices: ["system", "light", "dark"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            minimum: None,
            maximum: None,
            default: None,
            repeated: false,
        }),
        "navigate_search" => Some(ArgumentSchema {
            name: "direction".to_owned(),
            value_type: ValueType::Enum,
            required: true,
            choices: ["next", "previous"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            minimum: None,
            maximum: None,
            default: None,
            repeated: false,
        }),
        "copy_to_clipboard" => Some(ArgumentSchema {
            name: "format".to_owned(),
            value_type: ValueType::Enum,
            required: false,
            choices: ["plain", "vt", "html", "mixed"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            minimum: None,
            maximum: None,
            default: None,
            repeated: false,
        }),
        "csi" | "esc" | "text" | "search" => Some(argument("value", ValueType::String)),
        _ => None,
    };
    CompactSchema {
        arguments: argument.into_iter().collect(),
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
        default: None,
        repeated: false,
    }
}
fn legacy_target_for(id: &str) -> Option<ResourceKind> {
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
        "new_mux_session" | "session_picker" | "next_session" | "previous_session"
        | "last_session" | "select_session" | "close_space" | "edit_space" => {
            Some(ResourceKind::Binding)
        }
        "new_tab" | "rename_session" | "ditch_session" | "move_session" | "next_tab"
        | "previous_tab" | "last_tab" | "select_tab" => Some(ResourceKind::Session),
        "move_tab" | "rename_tab" => Some(ResourceKind::MuxWindow),
        "select_pane" | "next_pane" | "previous_pane" => Some(ResourceKind::Pane),
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
        | "text" => Some(ResourceKind::Terminal),
        _ => None,
    }
}

pub struct AppCommandRequest {
    pub invocation: CommandInvocation,
    pub deadline: Instant,
    pub cancellation: CommandCancellation,
    pub response: mpsc::Sender<CommandOutcome>,
    /// Optional event provenance for requests that cross the control socket. Requests
    /// produced by in-process callers leave this unset and do not publish a completion event.
    pub completion: Option<CommandCompletionContext>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandCompletionContext {
    pub caller: Caller,
    pub owner_pid: u32,
    pub owner_generation: u64,
    pub target: Option<CommandTarget>,
}

#[derive(Clone)]
pub struct AppCommandSender {
    sender: SyncSender<AppCommandRequest>,
    repaint: RepaintHandle,
    open: Arc<Mutex<bool>>,
}

#[derive(Clone)]
pub struct BoundAppCommandSender {
    sender: SyncSender<AppCommandRequest>,
    repaint: RepaintHandle,
    open: Arc<Mutex<bool>>,
    caller: Caller,
}

pub struct AppCommandReceiver {
    receiver: Receiver<AppCommandRequest>,
    open: Arc<Mutex<bool>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppCommandSendError {
    Overloaded,
    Shutdown,
}

pub fn app_command_channel(capacity: usize) -> (AppCommandSender, AppCommandReceiver) {
    app_command_channel_with_repaint(capacity, Arc::new(|| {}))
}

pub fn app_command_channel_with_repaint(
    capacity: usize,
    repaint: RepaintHandle,
) -> (AppCommandSender, AppCommandReceiver) {
    let (sender, receiver) = mpsc::sync_channel(capacity);
    let open = Arc::new(Mutex::new(true));
    (
        AppCommandSender {
            sender,
            repaint,
            open: open.clone(),
        },
        AppCommandReceiver { receiver, open },
    )
}

impl AppCommandSender {
    /// Binds the authenticated transport identity used for every submitted invocation.
    ///
    /// Submission is non-blocking and responses arrive asynchronously. Code already running on
    /// the AppState/UI owner thread must dispatch directly; waiting there for this channel would
    /// prevent the next frame from draining the request.
    pub fn for_caller(&self, caller: Caller) -> BoundAppCommandSender {
        BoundAppCommandSender {
            sender: self.sender.clone(),
            repaint: self.repaint.clone(),
            open: self.open.clone(),
            caller,
        }
    }
}

impl BoundAppCommandSender {
    pub fn try_send(&self, mut request: AppCommandRequest) -> Result<(), AppCommandSendError> {
        let open = self.open.lock().unwrap_or_else(|error| error.into_inner());
        if !*open {
            return Err(AppCommandSendError::Shutdown);
        }
        request.invocation.caller = self.caller;
        self.sender.try_send(request).map_err(|error| match error {
            TrySendError::Full(_) => AppCommandSendError::Overloaded,
            TrySendError::Disconnected(_) => AppCommandSendError::Shutdown,
        })?;
        (self.repaint)();
        Ok(())
    }
}

impl AppCommandReceiver {
    pub fn try_recv(&self) -> Result<AppCommandRequest, mpsc::TryRecvError> {
        self.receiver.try_recv()
    }
    /// Stop accepting new requests while the owner performs bounded teardown.
    pub fn close(&self) {
        let mut open = self.open.lock().unwrap_or_else(|error| error.into_inner());
        *open = false;
    }
}

impl Drop for AppCommandReceiver {
    fn drop(&mut self) {
        let mut open = self.open.lock().unwrap_or_else(|error| error.into_inner());
        *open = false;
        while let Ok(request) = self.receiver.try_recv() {
            let _ = request.response.send(CommandOutcome::Failed {
                code: "shutdown".to_owned(),
                message: "application command channel shut down".to_owned(),
            });
        }
    }
}

pub fn core_command_ids() -> BTreeSet<String> {
    CommandRegistry::core()
        .list()
        .map(|descriptor| descriptor.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_actions::{AppAction, MuxKeyAction, SidebarAction};
    #[test]
    fn invocation_preserves_parameterized_actions() {
        let invocation = CommandInvocation::from_action("move_tab:-1", Caller::Keybinding);
        let resolved = CommandRegistry::core().resolve(invocation).unwrap();

        assert_eq!(
            resolved.executor,
            CoreCommandExecutor::Keybind(KeybindAction::Mux(MuxKeyAction::MoveTab(-1)))
        );
    }

    #[test]
    fn catalog_command_resolves_through_the_same_registry() {
        let invocation =
            CommandInvocation::from_catalog(Command::ToggleSidebar, Caller::CommandPalette)
                .expect("palette command");
        let resolved = CommandRegistry::core().resolve(invocation).unwrap();

        assert_eq!(
            resolved.executor,
            CoreCommandExecutor::Keybind(KeybindAction::App(AppAction::ToggleSidebarVisibility))
        );
    }

    #[test]
    fn invocation_serialization_has_stable_field_names() {
        let invocation = CommandInvocation::from_action("next_tab", Caller::BuiltinKeybinding);

        assert_eq!(
            serde_json::to_value(invocation).unwrap(),
            serde_json::json!({
                "command": "next_tab",
                "caller": "builtin_keybinding"
            })
        );
    }

    #[test]
    fn palette_commands_come_from_the_registry() {
        let commands = CommandRegistry::core()
            .palette_commands()
            .collect::<Vec<_>>();

        assert!(commands.contains(&Command::ToggleSidebar));
        assert!(!commands.contains(&Command::SelectTab));
    }

    #[test]
    fn registry_lists_only_the_exact_canonical_catalog() {
        let ids = core_command_ids();

        assert_eq!(ids.len(), 302);
        assert_eq!(ids.len(), CommandRegistry::core().list().count());
        assert!(ids.contains("ui.sidebar.toggle"));
        assert!(ids.contains("window.move"));
        assert!(ids.contains("terminal.search"));
        assert!(ids.contains("agents.start"));
        assert!(!ids.contains("toggle_sidebar_visibility"));
        assert_eq!(
            CommandRegistry::core()
                .describe("toggle_sidebar_visibility")
                .map(|descriptor| descriptor.id),
            Some("ui.sidebar.toggle".to_owned())
        );
    }

    #[test]
    fn describe_preserves_the_effective_legacy_alias_schema() {
        let canonical = CommandRegistry::core()
            .describe("session.create")
            .expect("canonical session create descriptor");
        let alias = CommandRegistry::core()
            .describe("new_mux_session")
            .expect("legacy new session alias");

        assert_eq!(alias.id, canonical.id);
        assert_eq!(canonical.arguments.arguments[0].name, "launch");
        assert!(alias.arguments.arguments.is_empty());
    }

    #[test]
    fn canonical_session_select_uses_exact_string_selector_while_legacy_alias_stays_numeric() {
        let registry = CommandRegistry::core();
        let canonical = registry
            .resolve(CommandInvocation {
                command: "session.select".to_owned(),
                arguments: vec!["work".to_owned()],
                caller: Caller::Socket,
                target: None,
                confirmation: None,
            })
            .expect("canonical selector");
        let legacy = registry
            .resolve(CommandInvocation::from_action(
                "select_session:2",
                Caller::Keybinding,
            ))
            .expect("legacy ordinal selector");

        assert_eq!(canonical.descriptor.id, "session.select");
        assert_eq!(
            canonical.executor,
            CoreCommandExecutor::SessionSelect {
                selector: "work".to_owned(),
            }
        );
        assert_eq!(
            legacy.executor,
            CoreCommandExecutor::Keybind(KeybindAction::Mux(MuxKeyAction::SelectSession(2)))
        );
    }

    #[test]
    fn ambiguous_command_outcome_serializes_candidate_targets() {
        let outcome = CommandOutcome::Ambiguous {
            message: "selector collision".to_owned(),
            candidates: vec![CommandTarget {
                kind: ResourceKind::Session,
                handle: "opaque-session-target".to_owned(),
                generation: 7,
            }],
        };

        assert_eq!(
            serde_json::to_value(outcome).expect("serialize ambiguity outcome"),
            serde_json::json!({
                "status": "ambiguous",
                "message": "selector collision",
                "candidates": [{
                    "kind": "session",
                    "handle": "opaque-session-target",
                    "generation": "7"
                }]
            })
        );
    }

    #[test]
    fn pane_mux_descriptors_route_canonical_commands_and_aliases_to_typed_executors() {
        let registry = CommandRegistry::core();
        for (command, arguments, canonical, target, executor) in [
            (
                "pane.select",
                vec!["right".to_owned()],
                "pane.select",
                ResourceKind::Pane,
                CoreCommandExecutor::Mux(MuxCommandSpec::SelectPane {
                    direction: MuxDirection::Right,
                }),
            ),
            (
                "select-pane",
                vec!["left".to_owned()],
                "pane.select",
                ResourceKind::Pane,
                CoreCommandExecutor::Mux(MuxCommandSpec::SelectPane {
                    direction: MuxDirection::Left,
                }),
            ),
            (
                "pane.last",
                Vec::new(),
                "pane.last",
                ResourceKind::MuxWindow,
                CoreCommandExecutor::Mux(MuxCommandSpec::SelectLastPane),
            ),
            (
                "last-pane",
                Vec::new(),
                "pane.last",
                ResourceKind::MuxWindow,
                CoreCommandExecutor::Mux(MuxCommandSpec::SelectLastPane),
            ),
            (
                "pane.resize",
                vec![r#"{"kind":"directional","direction":"down","cells":3}"#.to_owned()],
                "pane.resize",
                ResourceKind::Pane,
                CoreCommandExecutor::Mux(MuxCommandSpec::ResizePane {
                    adjustment: MuxPaneResize::Directional {
                        direction: MuxDirection::Down,
                        cells: 3,
                    },
                }),
            ),
            (
                "resize-pane",
                vec![r#"{"kind":"absolute","columns":120,"rows":40}"#.to_owned()],
                "pane.resize",
                ResourceKind::Pane,
                CoreCommandExecutor::Mux(MuxCommandSpec::ResizePane {
                    adjustment: MuxPaneResize::Absolute {
                        columns: Some(120),
                        rows: Some(40),
                    },
                }),
            ),
        ] {
            let resolved = registry
                .resolve(CommandInvocation {
                    command: command.to_owned(),
                    arguments,
                    caller: Caller::Cli,
                    target: None,
                    confirmation: None,
                })
                .expect(command);

            assert_eq!(resolved.descriptor.id, canonical, "{command}");
            assert_eq!(resolved.descriptor.target, Some(target), "{command}");
            assert_eq!(resolved.executor, executor, "{command}");
        }
    }

    #[test]
    fn pane_mux_descriptor_availability_matches_backend_capabilities() {
        let registry = CommandRegistry::core();
        for (command, target, native, rmux, tmux) in [
            (
                "pane.select",
                ResourceKind::Pane,
                CatalogAvailability::Conditional,
                CatalogAvailability::Unavailable,
                CatalogAvailability::Conditional,
            ),
            (
                "pane.last",
                ResourceKind::MuxWindow,
                CatalogAvailability::Unsupported,
                CatalogAvailability::Unavailable,
                CatalogAvailability::Conditional,
            ),
            (
                "pane.resize",
                ResourceKind::Pane,
                CatalogAvailability::Unsupported,
                CatalogAvailability::Unavailable,
                CatalogAvailability::Conditional,
            ),
        ] {
            let descriptor = registry.describe(command).expect(command);
            let availability = descriptor.availability.expect("catalog availability");

            assert_eq!(descriptor.target, Some(target), "{command}");
            assert_eq!(
                availability.core,
                CatalogAvailability::Available,
                "{command}"
            );
            assert_eq!(availability.native, native, "{command}");
            assert_eq!(availability.rmux, rmux, "{command}");
            assert_eq!(availability.tmux, tmux, "{command}");
        }

        let select = registry.describe("pane.select").expect("pane.select");
        assert_eq!(select.arguments.arguments[0].name, "direction");
        assert_eq!(select.arguments.arguments[0].value_type, ValueType::Enum);

        let resize = registry.describe("pane.resize").expect("pane.resize");
        assert_eq!(resize.arguments.arguments[0].name, "adjustment");
        assert_eq!(resize.arguments.arguments[0].value_type, ValueType::Object);
    }
    #[test]
    fn pane_focus_uses_the_current_pane_and_app_window_open_is_unsupported() {
        let registry = CommandRegistry::core();
        for command in ["pane.focus", "ui.sidebar.focus_terminal"] {
            let resolved = registry
                .resolve(CommandInvocation::from_action(command, Caller::Internal))
                .expect(command);
            assert_eq!(resolved.descriptor.id, "pane.focus", "{command}");
            assert_eq!(
                resolved.descriptor.target,
                Some(ResourceKind::Pane),
                "{command}"
            );
            assert_eq!(
                resolved.executor,
                CoreCommandExecutor::Sidebar(SidebarAction::FocusTerminal),
                "{command}"
            );
        }
        assert!(matches!(
            registry.resolve(CommandInvocation::from_action(
                "app.window.open",
                Caller::Internal
            )),
            Err(CommandOutcome::Unsupported { .. })
        ));
    }

    #[test]
    fn parameterized_search_aliases_select_the_matching_canonical_descriptor() {
        let registry = CommandRegistry::core();

        for (action, canonical) in [
            ("navigate_search:next", "terminal.search.next"),
            ("navigate_search:previous", "terminal.search.previous"),
        ] {
            let resolved = registry
                .resolve(CommandInvocation::from_action(action, Caller::Keybinding))
                .expect("parameterized search action");
            assert_eq!(resolved.descriptor.id, canonical);
            assert_eq!(
                resolved.descriptor.arguments.arguments[0].value_type,
                ValueType::Enum
            );
        }
        assert_eq!(
            registry
                .describe("navigate_search:previous")
                .map(|descriptor| descriptor.id),
            Some("terminal.search.previous".to_owned())
        );
    }

    #[test]
    fn registry_preserves_schema_vocabulary_and_distinct_target_kinds() {
        for (catalog, runtime) in [
            (CatalogValueType::Null, ValueType::Null),
            (CatalogValueType::Boolean, ValueType::Boolean),
            (CatalogValueType::Integer, ValueType::Integer),
            (CatalogValueType::Number, ValueType::Number),
            (CatalogValueType::String, ValueType::String),
            (CatalogValueType::Enum, ValueType::Enum),
            (CatalogValueType::Array, ValueType::Array),
            (CatalogValueType::Object, ValueType::Object),
            (CatalogValueType::ResourceRef, ValueType::ResourceRef),
            (CatalogValueType::Json, ValueType::Json),
        ] {
            assert_eq!(value_type_from_catalog(catalog), runtime);
        }
        for (command, target) in [
            ("backend.list", ResourceKind::Binding),
            ("client.list", ResourceKind::Client),
            ("space.list", ResourceKind::Space),
            ("event.snapshot", ResourceKind::Subscription),
            ("ui.surface.list", ResourceKind::Surface),
            ("extension.list", ResourceKind::Extension),
        ] {
            assert_eq!(
                CommandRegistry::core()
                    .describe(command)
                    .expect("canonical descriptor")
                    .target,
                Some(target),
                "{command}"
            );
        }
    }

    #[test]
    fn registry_exposes_full_canonical_descriptor_metadata() {
        let descriptor = CommandRegistry::core()
            .describe("agents.start")
            .expect("agent descriptor");

        assert!(descriptor.aliases.contains(&"claude".to_owned()));
        assert!(matches!(
            &descriptor.origin,
            Some(CatalogOrigin::Extension { .. })
        ));
        assert!(descriptor.result_schema.is_some());
        assert_eq!(
            descriptor
                .availability
                .as_ref()
                .map(|availability| availability.core),
            Some(CatalogAvailability::Unavailable)
        );
        assert_eq!(
            descriptor
                .palette_metadata
                .as_ref()
                .map(|palette| palette.category.as_str()),
            Some("agents")
        );
    }

    #[test]
    fn sidebar_commands_have_window_scoped_typed_executors() {
        for action in SidebarAction::ALL {
            let resolved = CommandRegistry::core()
                .resolve(CommandInvocation::from_action(
                    action.command_id(),
                    Caller::Keybinding,
                ))
                .unwrap();

            assert_eq!(
                resolved.descriptor.target,
                Some(if action == SidebarAction::FocusTerminal {
                    ResourceKind::Pane
                } else {
                    ResourceKind::ApplicationWindow
                })
            );
            assert_eq!(resolved.executor, CoreCommandExecutor::Sidebar(action));
        }
    }

    #[test]
    fn schema_rejects_bad_parameter_types() {
        let invocation = CommandInvocation::from_action("select_tab:nope", Caller::Cli);
        assert!(matches!(
            CommandRegistry::core().resolve(invocation),
            Err(CommandOutcome::Failed { code, .. }) if code == "invalid_arguments"
        ));
    }

    #[test]
    fn catalog_only_commands_are_typed_not_unknown() {
        let catalog_only = CommandInvocation {
            command: "layout.apply".to_owned(),
            arguments: Vec::new(),
            caller: Caller::Socket,
            target: None,
            confirmation: None,
        };
        let extension_placeholder = CommandInvocation {
            command: "agents.start".to_owned(),
            arguments: Vec::new(),
            caller: Caller::Socket,
            target: None,
            confirmation: None,
        };

        assert!(matches!(
            CommandRegistry::core().resolve(catalog_only),
            Err(CommandOutcome::Unsupported { .. })
        ));
        assert!(matches!(
            CommandRegistry::core().resolve(extension_placeholder),
            Err(CommandOutcome::Unavailable { .. })
        ));
    }

    #[test]
    fn explicit_unsupported_and_direct_control_paths_do_not_fake_execution() {
        assert!(matches!(
            CommandRegistry::core().resolve(CommandInvocation {
                command: "pane.neighbor".to_owned(),
                arguments: vec!["right".to_owned()],
                caller: Caller::Socket,
                target: None,
                confirmation: None,
            }),
            Err(CommandOutcome::Unsupported { .. })
        ));
        assert!(matches!(
            CommandRegistry::core().resolve(CommandInvocation {
                command: "event.snapshot".to_owned(),
                arguments: vec!["subscription-1".to_owned()],
                caller: Caller::Socket,
                target: None,
                confirmation: None,
            }),
            Err(CommandOutcome::Failed { code, .. }) if code == "direct_control_only"
        ));
    }

    #[test]
    fn worktree_commands_resolve_to_typed_core_executors() {
        let registry = CommandRegistry::core();
        let create = registry
            .resolve(CommandInvocation {
                command: "worktree.create".to_owned(),
                arguments: vec!["/repo".to_owned(), "feature/catalog".to_owned()],
                caller: Caller::Socket,
                target: None,
                confirmation: None,
            })
            .expect("worktree.create");
        assert_eq!(
            create.executor,
            CoreCommandExecutor::WorktreeCreate {
                repository_path: "/repo".to_owned(),
                branch: "feature/catalog".to_owned(),
                managed_by_bootty: true,
            }
        );

        let remove = registry
            .resolve(CommandInvocation {
                command: "worktree.remove".to_owned(),
                arguments: vec!["/repo/.worktrees/catalog".to_owned()],
                caller: Caller::Socket,
                target: None,
                confirmation: None,
            })
            .expect("worktree.remove");
        assert_eq!(
            remove.executor,
            CoreCommandExecutor::WorktreeRemove {
                path: "/repo/.worktrees/catalog".to_owned(),
                force: false,
                confirmation: None,
            }
        );
    }

    #[test]
    fn canonical_session_create_parses_the_object_launch_schema() {
        let invocation = CommandInvocation {
            command: "session.create".to_owned(),
            arguments: vec![r#"{"name":"catalog"}"#.to_owned()],
            caller: Caller::Socket,
            target: None,
            confirmation: None,
        };

        assert!(matches!(
            CommandRegistry::core().resolve(invocation),
            Ok(ResolvedCommandInvocation {
                executor: CoreCommandExecutor::SessionCreate(_),
                ..
            })
        ));
        assert!(matches!(
            CommandRegistry::core().resolve(CommandInvocation {
                command: "session.create".to_owned(),
                arguments: vec!["[]".to_owned()],
                caller: Caller::Socket,
                target: None,
                confirmation: None,
            }),
            Err(CommandOutcome::Failed { code, .. }) if code == "invalid_arguments"
        ));
    }

    #[test]
    fn session_create_rejects_unknown_nested_launch_fields() {
        let invocation = CommandInvocation {
            command: "session.create".to_owned(),
            arguments: vec![
                r#"{"default_cwd":"/repo","windows":[{"layout":{"kind":"pane","command":"exec ./serve","unknown":true}}]}"#
                    .to_owned(),
            ],
            caller: Caller::Socket,
            target: None,
            confirmation: None,
        };

        assert!(matches!(
            CommandRegistry::core().resolve(invocation),
            Err(CommandOutcome::Failed { code, .. }) if code == "invalid_arguments"
        ));
    }

    #[test]
    fn callers_resolve_the_same_command_contract() {
        let callers = [
            Caller::CommandPalette,
            Caller::Keybinding,
            Caller::BuiltinKeybinding,
            Caller::Cli,
            Caller::Socket,
            Caller::Luau,
            Caller::Internal,
        ];
        let executors = callers.map(|caller| {
            CommandRegistry::core()
                .resolve(CommandInvocation::from_action("next_tab", caller))
                .unwrap()
                .executor
        });

        assert!(executors.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(
            executors[0],
            CoreCommandExecutor::Keybind(KeybindAction::Mux(MuxKeyAction::NextTab))
        );
    }

    #[test]
    fn descriptors_declare_current_context_targets() {
        assert_eq!(
            CommandRegistry::core()
                .describe("kill_pane")
                .unwrap()
                .target,
            Some(ResourceKind::Pane)
        );
        assert_eq!(
            CommandRegistry::core()
                .describe("close_window")
                .unwrap()
                .target,
            Some(ResourceKind::ApplicationWindow)
        );
        assert_eq!(
            CommandRegistry::core()
                .describe("ditch_session")
                .unwrap()
                .mutation,
            MutationClass::Destructive
        );
        assert_eq!(
            CommandRegistry::core()
                .describe("ditch_session")
                .unwrap()
                .target,
            Some(ResourceKind::Session)
        );
        for (action, target) in [
            ("new_tab", ResourceKind::Session),
            ("rename_tab", ResourceKind::MuxWindow),
            ("move_session:1", ResourceKind::Session),
            ("select_pane:right", ResourceKind::Pane),
            ("next_pane", ResourceKind::Pane),
            ("previous_pane", ResourceKind::Pane),
            ("copy_mode", ResourceKind::Terminal),
        ] {
            assert_eq!(
                CommandRegistry::core()
                    .resolve(CommandInvocation::from_action(action, Caller::Internal))
                    .unwrap()
                    .descriptor
                    .target,
                Some(target),
                "{action}"
            );
        }
        assert!(matches!(
            CommandRegistry::core()
                .resolve(CommandInvocation::from_action("new_window", Caller::Internal)),
            Err(CommandOutcome::Failed { code, .. }) if code == "unknown_command"
        ));
    }

    #[test]
    fn confirmation_is_bound_to_the_exact_target_generation() {
        let mut invocation = CommandInvocation::from_action("kill_pane", Caller::Socket);
        invocation.target = Some(CommandTarget {
            kind: ResourceKind::Pane,
            handle: "binding:1/pane:%2".to_owned(),
            generation: 4,
        });
        let confirmation = invocation.confirmation();
        invocation.target.as_mut().unwrap().generation = 5;

        assert_ne!(confirmation, invocation.confirmation());
    }

    #[test]
    fn target_generations_round_trip_as_exact_decimal_strings() {
        let target = CommandTarget {
            kind: ResourceKind::Instance,
            handle: "instance".to_owned(),
            generation: u64::MAX,
        };

        let json = serde_json::to_value(&target).unwrap();
        assert_eq!(json["generation"], u64::MAX.to_string());
        assert_eq!(
            serde_json::from_value::<CommandTarget>(json).unwrap(),
            target
        );
    }

    #[test]
    fn cancellation_is_shared_with_the_queued_request() {
        let cancellation = CommandCancellation::new();
        let queued = cancellation.clone();

        cancellation.cancel();

        assert!(queued.is_cancelled());
    }

    #[test]
    fn cancellation_cannot_override_started_dispatch() {
        let cancellation = CommandCancellation::new();

        assert!(cancellation.try_start());
        cancellation.cancel();

        assert!(!cancellation.is_cancelled());
    }

    #[test]
    fn app_command_channel_is_bounded() {
        let (tx, _rx) = app_command_channel(1);
        let tx = tx.for_caller(Caller::Socket);
        let request = || {
            let (response, _) = mpsc::channel();
            AppCommandRequest {
                invocation: CommandInvocation::from_action("next_tab", Caller::Socket),
                deadline: Instant::now(),
                cancellation: CommandCancellation::new(),
                response,
                completion: None,
            }
        };

        tx.try_send(request()).unwrap();
        assert_eq!(tx.try_send(request()), Err(AppCommandSendError::Overloaded));
    }

    #[test]
    fn app_command_send_wakes_the_ui_thread() {
        let (wake_tx, wake_rx) = mpsc::channel();
        let repaint: RepaintHandle = Arc::new(move || {
            let _ = wake_tx.send(());
        });
        let (tx, _rx) = app_command_channel_with_repaint(1, repaint);
        let tx = tx.for_caller(Caller::Socket);
        let (response, _) = mpsc::channel();

        tx.try_send(AppCommandRequest {
            invocation: CommandInvocation::from_action("next_tab", Caller::Socket),
            deadline: Instant::now(),
            cancellation: CommandCancellation::new(),
            response,
            completion: None,
        })
        .unwrap();

        wake_rx.try_recv().unwrap();
    }

    #[test]
    fn app_command_sender_binds_the_trusted_caller() {
        let (tx, rx) = app_command_channel(1);
        let tx = tx.for_caller(Caller::Socket);
        let (response, _) = mpsc::channel();

        tx.try_send(AppCommandRequest {
            invocation: CommandInvocation::from_action("next_tab", Caller::Internal),
            deadline: Instant::now(),
            cancellation: CommandCancellation::new(),
            response,
            completion: None,
        })
        .unwrap();

        assert_eq!(rx.try_recv().unwrap().invocation.caller, Caller::Socket);
    }

    #[test]
    fn app_command_channel_reports_shutdown() {
        let (tx, rx) = app_command_channel(1);
        let tx = tx.for_caller(Caller::Socket);
        drop(rx);
        let (response, _) = mpsc::channel();

        assert_eq!(
            tx.try_send(AppCommandRequest {
                invocation: CommandInvocation::from_action("next_tab", Caller::Socket),
                deadline: Instant::now(),
                cancellation: CommandCancellation::new(),
                response,
                completion: None,
            }),
            Err(AppCommandSendError::Shutdown)
        );
    }

    #[test]
    fn dropping_the_command_receiver_completes_queued_requests() {
        let (tx, rx) = app_command_channel(1);
        let tx = tx.for_caller(Caller::Socket);
        let (response, response_rx) = mpsc::channel();
        tx.try_send(AppCommandRequest {
            invocation: CommandInvocation::from_action("next_tab", Caller::Internal),
            deadline: Instant::now(),
            cancellation: CommandCancellation::new(),
            response,
            completion: None,
        })
        .unwrap();

        drop(rx);

        assert!(matches!(
            response_rx.recv().unwrap(),
            CommandOutcome::Failed { code, .. } if code == "shutdown"
        ));
    }
}
