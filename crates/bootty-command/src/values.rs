use serde::{Deserialize, Serialize};
use serde_json::Value;

mod decimal_u64 {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S, T>(value: &T, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        T: Copy + Into<u64>,
    {
        serializer.serialize_str(&(*value).into().to_string())
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

    pub fn confirmation(&self) -> Confirmation {
        Confirmation {
            command: self.command.clone(),
            arguments: self.arguments.clone(),
            target: self.target.clone(),
        }
    }

    pub fn action_name(&self) -> String {
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
