use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, OnceLock,
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
    ReadTerminal,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedCommandInvocation {
    pub descriptor: CommandDescriptor,
    pub executor: CoreCommandExecutor,
    pub invocation: CommandInvocation,
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
            CommandExecutorResolver::ReadTerminal => CoreCommandExecutor::ReadTerminal,
            CommandExecutorResolver::WriteTerminal => CoreCommandExecutor::Keybind(
                KeybindAction::Write(invocation.arguments[0].as_bytes().to_vec()),
            ),
        };
        Ok(ResolvedCommandInvocation {
            descriptor,
            executor,
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
        for (descriptor, executor) in [
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

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
    fn registry_has_one_descriptor_per_command_id() {
        let ids = CommandRegistry::core()
            .list()
            .map(|descriptor| descriptor.id.clone())
            .collect::<BTreeSet<String>>();
        assert_eq!(ids.len(), CommandRegistry::core().list().count());
        assert!(ids.contains("toggle_sidebar_visibility"));
        assert!(ids.contains("move_tab"));
        assert!(ids.contains("search"));
        assert!(ids.contains("search_selection"));
        assert!(ids.contains("navigate_search"));
        assert!(ids.contains("end_search"));
        assert!(ids.contains("ui.sidebar.activate_session"));
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
                Some(ResourceKind::ApplicationWindow)
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
        for (command, target) in [
            ("new_window", ResourceKind::Binding),
            ("new_tab", ResourceKind::Session),
            ("rename_tab", ResourceKind::MuxWindow),
            ("copy_mode", ResourceKind::Terminal),
        ] {
            assert_eq!(
                CommandRegistry::core().describe(command).unwrap().target,
                Some(target),
                "{command}"
            );
        }
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
        })
        .unwrap();

        drop(rx);

        assert!(matches!(
            response_rx.recv().unwrap(),
            CommandOutcome::Failed { code, .. } if code == "shutdown"
        ));
    }
}
