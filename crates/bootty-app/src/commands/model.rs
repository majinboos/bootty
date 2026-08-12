use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    action_catalog::Command,
    app_actions::{KeybindAction, SidebarAction},
    automation::{directory::WorktreeRemovalConfirmation, launch::SessionLaunchDescriptor},
    mux::command::{MuxDirection, MuxPaneResize},
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandOrigin {
    Core,
    Extension {
        extension_id: String,
        generation: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandResultType {
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
pub struct CommandResultSchema {
    #[serde(rename = "type")]
    pub value_type: CommandResultType,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, CommandResultSchema>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<CommandResultSchema>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandTargetKind {
    Instance,
    ApplicationWindow,
    Binding,
    Space,
    Session,
    Window,
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
pub struct BackendAvailability {
    pub core: CommandAvailability,
    pub native: CommandAvailability,
    pub rmux: CommandAvailability,
    pub tmux: CommandAvailability,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandAvailability {
    Available,
    Conditional,
    Unsupported,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandPaletteMetadata {
    pub visible: bool,
    pub category: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
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
    pub origin: Option<CommandOrigin>,
    pub mutation: MutationClass,
    pub arguments: CompactSchema,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_schema: Option<CommandResultSchema>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<CommandTargetKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<BackendAvailability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<ResourceKind>,
    #[serde(default)]
    pub palette: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub palette_metadata: Option<CommandPaletteMetadata>,
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
