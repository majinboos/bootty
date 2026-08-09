use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    time::Instant,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    action_catalog::Command,
    app_actions::{KeybindAction, keybind_action_for_name},
    mux::RepaintHandle,
};

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
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedCommandInvocation {
    pub descriptor: CommandDescriptor,
    pub action: KeybindAction,
    pub invocation: CommandInvocation,
}

#[derive(Clone, Debug, Default)]
pub struct CommandRegistry {
    descriptors: BTreeMap<String, CommandDescriptor>,
}

impl CommandRegistry {
    pub fn core() -> &'static Self {
        static REGISTRY: OnceLock<CommandRegistry> = OnceLock::new();
        REGISTRY.get_or_init(Self::from_action_catalog)
    }

    pub fn list(&self) -> impl Iterator<Item = &CommandDescriptor> {
        self.descriptors.values()
    }

    pub fn describe(&self, id: &str) -> Option<&CommandDescriptor> {
        self.descriptors.get(id)
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
        let Some(descriptor) = self.describe(&invocation.command).cloned() else {
            return Err(CommandOutcome::Failed {
                code: "unknown_command".to_owned(),
                message: format!("unknown command {}", invocation.command),
            });
        };
        validate_arguments(&descriptor, &invocation.arguments)?;
        let Some(action) = keybind_action_for_name(&invocation.action_name()) else {
            return Err(CommandOutcome::Unsupported {
                message: format!("command {} has no app executor", invocation.command),
            });
        };
        Ok(ResolvedCommandInvocation {
            descriptor,
            action,
            invocation,
        })
    }

    fn from_action_catalog() -> Self {
        let mut descriptors = BTreeMap::new();
        for command in Command::all() {
            let action = command.action();
            let id = action.split_once(':').map_or(action, |(id, _)| id);
            let descriptor = CommandDescriptor {
                id: id.to_owned(),
                title: command.title().to_owned(),
                description: command.description().to_owned(),
                mutation: mutation_for(id),
                arguments: schema_for(id),
                target: target_for(id),
                palette: command.palette_action().is_some(),
            };
            descriptors
                .entry(id.to_owned())
                .and_modify(|existing: &mut CommandDescriptor| {
                    existing.palette |= descriptor.palette;
                })
                .or_insert(descriptor);
        }
        Self { descriptors }
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
        let valid = match schema.value_type {
            ValueType::String => true,
            ValueType::Integer => value.parse::<i64>().is_ok(),
            ValueType::Number => value.parse::<f64>().is_ok(),
        } && (schema.choices.is_empty() || schema.choices.contains(value));
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
            Some(argument("index", ValueType::Integer))
        }
        "move_tab" | "move_session" | "scroll_page_lines" => {
            Some(argument("delta", ValueType::Integer))
        }
        "increase_font_size" | "decrease_font_size" | "set_font_size" => {
            Some(argument("size", ValueType::Number))
        }
        "select_pane" => Some(ArgumentSchema {
            name: "direction".to_owned(),
            value_type: ValueType::String,
            required: true,
            choices: ["left", "right", "up", "down"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        }),
        "change_appearance" => Some(ArgumentSchema {
            name: "appearance".to_owned(),
            value_type: ValueType::String,
            required: true,
            choices: ["system", "light", "dark"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        }),
        "navigate_search" => Some(ArgumentSchema {
            name: "direction".to_owned(),
            value_type: ValueType::String,
            required: true,
            choices: ["next", "previous"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
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
    }
}

fn mutation_for(id: &str) -> MutationClass {
    const DESTRUCTIVE: &[&str] = &[
        "close_space",
        "close_surface",
        "close_window",
        "kill_pane",
        "quit",
    ];
    const READ_ONLY: &[&str] = &["show_keybinds"];
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
        "new_window"
        | "close_window"
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
        | "command_palette" => Some(ResourceKind::ApplicationWindow),
        "new_mux_session" | "session_picker" | "close_space" | "edit_space" | "next_session"
        | "previous_session" | "last_session" | "select_session" => Some(ResourceKind::Binding),
        "rename_session" | "ditch_session" | "move_session" | "new_tab" | "next_tab"
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
        | "increase_font_size"
        | "decrease_font_size"
        | "reset_font_size"
        | "set_font_size"
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

#[derive(Clone, Debug, Default)]
pub struct CommandCancellation(Arc<AtomicBool>);

impl CommandCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
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
}

pub struct AppCommandReceiver(Receiver<AppCommandRequest>);

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
    (
        AppCommandSender { sender, repaint },
        AppCommandReceiver(receiver),
    )
}

impl AppCommandSender {
    pub fn try_send(&self, request: AppCommandRequest) -> Result<(), AppCommandSendError> {
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
        self.0.try_recv()
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
    use crate::app_actions::{AppAction, MuxKeyAction};

    #[test]
    fn invocation_preserves_parameterized_actions() {
        let invocation = CommandInvocation::from_action("move_tab:-1", Caller::Keybinding);
        let resolved = CommandRegistry::core().resolve(invocation).unwrap();

        assert_eq!(resolved.descriptor.id, "move_tab");
        assert_eq!(
            resolved.action,
            KeybindAction::Mux(MuxKeyAction::MoveTab(-1))
        );
    }

    #[test]
    fn catalog_command_resolves_through_the_same_registry() {
        let invocation =
            CommandInvocation::from_catalog(Command::ToggleSidebar, Caller::CommandPalette)
                .expect("palette command");
        let resolved = CommandRegistry::core().resolve(invocation).unwrap();

        assert_eq!(resolved.descriptor.id, "toggle_sidebar_visibility");
        assert_eq!(
            resolved.action,
            KeybindAction::App(AppAction::ToggleSidebarVisibility)
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
        let ids = core_command_ids();
        assert_eq!(ids.len(), CommandRegistry::core().list().count());
        assert!(ids.contains("toggle_sidebar_visibility"));
        assert!(ids.contains("move_tab"));
        assert!(ids.contains("search"));
        assert!(ids.contains("search_selection"));
        assert!(ids.contains("navigate_search"));
        assert!(ids.contains("end_search"));
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
        let actions = callers.map(|caller| {
            CommandRegistry::core()
                .resolve(CommandInvocation::from_action("next_tab", caller))
                .unwrap()
                .action
        });

        assert!(actions.windows(2).all(|pair| pair[0] == pair[1]));
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
    fn cancellation_is_shared_with_the_queued_request() {
        let cancellation = CommandCancellation::new();
        let queued = cancellation.clone();

        cancellation.cancel();

        assert!(queued.is_cancelled());
    }

    #[test]
    fn app_command_channel_is_bounded() {
        let (tx, _rx) = app_command_channel(1);
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
    fn app_command_channel_reports_shutdown() {
        let (tx, rx) = app_command_channel(1);
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
}
