use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicBool, Ordering},
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
    command_extensions::{ModuleIdentity, PublishedSurfaceSnapshot, SurfaceSnapshot},
    mux::RepaintHandle,
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
    String,
    Integer,
    Number,
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
    pub mutation: MutationClass,
    pub arguments: CompactSchema,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<ResourceKind>,
    #[serde(default)]
    pub palette: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Instance,
    ApplicationWindow,
    Binding,
    Session,
    MuxWindow,
    Pane,
    Terminal,
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
    pub fn new(command: impl Into<String>, arguments: Vec<String>, caller: Caller) -> Self {
        Self {
            command: command.into(),
            arguments,
            caller,
            target: None,
            confirmation: None,
        }
    }

    // ponytail: action-string arguments bridge existing keybindings; replace them with schema values
    // when the external command parser lands.
    pub fn from_action(action: &str, caller: Caller) -> Self {
        let (command, arguments) = action
            .split_once(':')
            .map_or((action, Vec::new()), |(command, arguments)| {
                (command, vec![arguments.to_owned()])
            });
        Self::new(command, arguments, caller)
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

    fn action_name(&self) -> String {
        match self.arguments.as_slice() {
            [] => self.command.clone(),
            arguments => format!("{}:{}", self.command, arguments.join(":")),
        }
    }
}

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

    pub fn cancelled() -> Self {
        Self::Failed {
            code: "cancelled".to_owned(),
            message: "command was cancelled".to_owned(),
        }
    }

    pub fn deadline_exceeded() -> Self {
        Self::Failed {
            code: "deadline_exceeded".to_owned(),
            message: "command deadline expired".to_owned(),
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

pub type ExtensionCommandHandler = Arc<
    dyn Fn(CommandInvocation, Instant, CommandCancellation) -> Receiver<CommandOutcome>
        + Send
        + Sync
        + 'static,
>;

#[derive(Clone, Debug)]
pub struct ExtensionGenerationToken(Arc<AtomicBool>);

impl ExtensionGenerationToken {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }

    pub fn is_active(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    pub(crate) fn retire(&self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Default for ExtensionGenerationToken {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
struct ExtensionCommand {
    module: String,
    generation: u64,
    descriptor: CommandDescriptor,
    handler: ExtensionCommandHandler,
}

#[derive(Clone)]
struct ExtensionTopic {
    module: String,
    generation: u64,
}

#[derive(Clone)]
struct ExtensionSurface {
    module: String,
    generation: u64,
    snapshot: SurfaceSnapshot,
}

#[derive(Default)]
struct ExtensionCatalog {
    commands: BTreeMap<String, ExtensionCommand>,
    topics: BTreeMap<String, ExtensionTopic>,
    surfaces: BTreeMap<(String, String), ExtensionSurface>,
    generations: BTreeMap<String, (u64, ExtensionGenerationToken)>,
}

#[derive(Clone)]
pub enum CommandExecutor {
    Core(CoreCommandExecutor),
    Extension(ExtensionCommandHandler),
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
    extensions: Arc<RwLock<ExtensionCatalog>>,
}

pub struct ExtensionGenerationCandidate {
    pub identity: ModuleIdentity,
    pub generation: u64,
    pub token: ExtensionGenerationToken,
    pub commands: Vec<(CommandDescriptor, ExtensionCommandHandler)>,
    pub topics: Vec<String>,
    pub surfaces: Vec<SurfaceSnapshot>,
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
        Self {
            core: CommandRegistry::core(),
            extensions: Arc::default(),
        }
    }
}

impl CommandCatalog {
    pub fn list(&self) -> Vec<CommandDescriptor> {
        let mut commands = self.core.list().cloned().collect::<Vec<_>>();
        if let Ok(extensions) = self.extensions.read() {
            commands.extend(
                extensions
                    .commands
                    .values()
                    .map(|command| command.descriptor.clone()),
            );
        }
        commands.sort_by(|left, right| left.id.cmp(&right.id));
        commands
    }

    pub fn describe(&self, id: &str) -> Option<CommandDescriptor> {
        self.core.describe(id).cloned().or_else(|| {
            self.extensions.read().ok().and_then(|catalog| {
                catalog
                    .commands
                    .get(id)
                    .map(|command| command.descriptor.clone())
            })
        })
    }

    pub fn resolve(
        &self,
        invocation: CommandInvocation,
    ) -> Result<ResolvedCommandInvocation, CommandOutcome> {
        let command = self
            .extensions
            .read()
            .ok()
            .and_then(|catalog| catalog.commands.get(&invocation.command).cloned());
        if let Some(command) = command {
            validate_arguments(&command.descriptor, &invocation.arguments)?;
            return Ok(ResolvedCommandInvocation {
                descriptor: command.descriptor,
                invocation,
                executor: CommandExecutor::Extension(command.handler),
            });
        }
        self.core.resolve(invocation)
    }

    pub fn publish_extension_generation(
        &self,
        candidate: ExtensionGenerationCandidate,
    ) -> Result<(), String> {
        let ExtensionGenerationCandidate {
            identity,
            generation,
            token,
            commands,
            topics,
            surfaces,
        } = candidate;
        let module = identity.as_str();
        let namespace = identity.namespace();
        let mut command_ids = BTreeSet::new();
        for (descriptor, _) in &commands {
            if !is_namespaced(&descriptor.id, &namespace) {
                return Err("extension command must be namespaced by its module".to_owned());
            }
            if self.core.describe(&descriptor.id).is_some() {
                return Err("extension command cannot replace a built-in command".to_owned());
            }
            if !command_ids.insert(descriptor.id.clone()) {
                return Err(format!(
                    "command {} is registered more than once",
                    descriptor.id
                ));
            }
        }
        let mut topic_ids = BTreeSet::new();
        for topic in &topics {
            if !is_namespaced(topic, &namespace) {
                return Err("extension event topic must be namespaced by its module".to_owned());
            }
            if !topic_ids.insert(topic.clone()) {
                return Err(format!("event topic {topic} is registered more than once"));
            }
        }
        let mut surface_ids = BTreeSet::new();
        for surface in &surfaces {
            if surface.declaration.id.is_empty() {
                return Err("extension surface identity is invalid".to_owned());
            }
            if !surface_ids.insert(surface.declaration.id.clone()) {
                return Err(format!(
                    "surface {} is registered more than once",
                    surface.declaration.id
                ));
            }
        }

        let mut catalog = self
            .extensions
            .write()
            .map_err(|_| "command catalog is unavailable".to_owned())?;
        for command in &command_ids {
            if catalog
                .commands
                .get(command)
                .is_some_and(|registered| registered.module != module)
            {
                return Err(format!("command {command} is already registered"));
            }
        }
        for topic in &topic_ids {
            if catalog
                .topics
                .get(topic)
                .is_some_and(|registered| registered.module != module)
            {
                return Err(format!("event topic {topic} is already registered"));
            }
        }
        for surface in &surfaces {
            if catalog.surfaces.values().any(|registered| {
                registered.module != module
                    && registered.snapshot.declaration.placement == surface.declaration.placement
                    && registered.snapshot.declaration.id == surface.declaration.id
            }) {
                return Err(format!(
                    "surface {} is already registered for {:?}",
                    surface.declaration.id, surface.declaration.placement
                ));
            }
        }

        if let Some((_, previous)) = catalog.generations.get(module) {
            previous.retire();
        }
        catalog
            .commands
            .retain(|_, command| command.module != module);
        catalog.topics.retain(|_, topic| topic.module != module);
        catalog
            .surfaces
            .retain(|_, surface| surface.module != module);
        for (descriptor, handler) in commands {
            catalog.commands.insert(
                descriptor.id.clone(),
                ExtensionCommand {
                    module: module.to_owned(),
                    generation,
                    descriptor,
                    handler,
                },
            );
        }
        for topic in topics {
            catalog.topics.insert(
                topic,
                ExtensionTopic {
                    module: module.to_owned(),
                    generation,
                },
            );
        }
        for snapshot in surfaces {
            catalog.surfaces.insert(
                (module.to_owned(), snapshot.declaration.id.clone()),
                ExtensionSurface {
                    module: module.to_owned(),
                    generation,
                    snapshot,
                },
            );
        }
        catalog
            .generations
            .insert(module.to_owned(), (generation, token));
        Ok(())
    }

    pub fn remove_extension_generation(&self, module: &str, generation: u64) {
        if let Ok(mut catalog) = self.extensions.write() {
            let matches = catalog
                .generations
                .get(module)
                .is_some_and(|(active, _)| *active == generation);
            if !matches {
                return;
            }
            if let Some((_, token)) = catalog.generations.remove(module) {
                token.retire();
            }
            catalog
                .commands
                .retain(|_, command| command.module != module || command.generation != generation);
            catalog
                .topics
                .retain(|_, topic| topic.module != module || topic.generation != generation);
            catalog
                .surfaces
                .retain(|_, surface| surface.module != module || surface.generation != generation);
        }
    }

    pub fn extension_topics(&self) -> BTreeSet<String> {
        self.extensions
            .read()
            .map(|catalog| catalog.topics.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub fn extension_surfaces(&self) -> Vec<PublishedSurfaceSnapshot> {
        self.extensions
            .read()
            .map(|catalog| {
                catalog
                    .surfaces
                    .values()
                    .map(|surface| PublishedSurfaceSnapshot {
                        module: surface.module.clone(),
                        generation: surface.generation,
                        snapshot: surface.snapshot.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn publish_extension_surfaces(
        &self,
        module: &str,
        generation: u64,
        surfaces: Vec<SurfaceSnapshot>,
    ) -> Result<(), String> {
        let mut surface_ids = BTreeSet::new();
        for surface in &surfaces {
            if surface.declaration.id.is_empty() {
                return Err("extension surface identity is invalid".to_owned());
            }
            if !surface_ids.insert(surface.declaration.id.clone()) {
                return Err(format!(
                    "surface {} is registered more than once",
                    surface.declaration.id
                ));
            }
        }
        let mut catalog = self
            .extensions
            .write()
            .map_err(|_| "command catalog is unavailable".to_owned())?;
        let generation_is_active = catalog
            .generations
            .get(module)
            .is_some_and(|(active, token)| *active == generation && token.is_active());
        if !generation_is_active {
            return Err("extension generation is not active".to_owned());
        }
        for surface in &surfaces {
            if catalog.surfaces.values().any(|registered| {
                registered.module != module
                    && registered.snapshot.declaration.placement == surface.declaration.placement
                    && registered.snapshot.declaration.id == surface.declaration.id
            }) {
                return Err(format!(
                    "surface {} is already registered for {:?}",
                    surface.declaration.id, surface.declaration.placement
                ));
            }
        }
        catalog
            .surfaces
            .retain(|_, surface| surface.module != module);
        for snapshot in surfaces {
            catalog.surfaces.insert(
                (module.to_owned(), snapshot.declaration.id.clone()),
                ExtensionSurface {
                    module: module.to_owned(),
                    generation,
                    snapshot,
                },
            );
        }
        Ok(())
    }

    pub(crate) fn with_active_extension_topic<T>(
        &self,
        module: &str,
        generation: u64,
        topic: &str,
        publish: impl FnOnce() -> T,
    ) -> Result<T, String> {
        let catalog = self
            .extensions
            .read()
            .map_err(|_| "command catalog is unavailable".to_owned())?;
        let generation_is_active = catalog
            .generations
            .get(module)
            .is_some_and(|(active, token)| *active == generation && token.is_active());
        let topic_is_active = catalog.topics.get(topic).is_some_and(|registered| {
            registered.module == module && registered.generation == generation
        });
        if !generation_is_active || !topic_is_active {
            return Err("extension event topic is not active".to_owned());
        }
        Ok(publish())
    }

    pub(crate) fn with_active_extension_generation<T>(
        &self,
        module: &str,
        generation: u64,
        action: impl FnOnce() -> T,
    ) -> Result<T, String> {
        let catalog = self
            .extensions
            .read()
            .map_err(|_| "command catalog is unavailable".to_owned())?;
        let generation_is_active = catalog
            .generations
            .get(module)
            .is_some_and(|(active, token)| *active == generation && token.is_active());
        if !generation_is_active {
            return Err("extension generation is not active".to_owned());
        }
        Ok(action())
    }
}

/// Whether `id` is `namespace` followed by a dot and a leaf, the namespacing every
/// extension-supplied command id and event topic must satisfy.
pub fn is_namespaced(id: &str, namespace: &str) -> bool {
    id.starts_with(namespace) && id[namespace.len()..].starts_with('.')
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

pub struct AppCommandRequest {
    pub invocation: CommandInvocation,
    pub deadline: Instant,
    pub cancellation: CommandCancellation,
    pub response: mpsc::Sender<CommandOutcome>,
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
    pub fn submit(
        &self,
        invocation: CommandInvocation,
        deadline: Instant,
        cancellation: CommandCancellation,
    ) -> Result<Receiver<CommandOutcome>, AppCommandSendError> {
        let (response, receiver) = mpsc::channel();
        self.try_send(AppCommandRequest {
            invocation,
            deadline,
            cancellation,
            response,
        })?;
        Ok(receiver)
    }

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
