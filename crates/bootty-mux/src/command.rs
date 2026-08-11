use std::{collections::BTreeMap, fmt, io::Write};

use serde::{Deserialize, Serialize};

#[cfg(feature = "app")]
use crate::capability::BindingOperation;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum MuxDirection {
    Left,
    Down,
    Up,
    Right,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum MuxSplitDirection {
    Right,
    Down,
}

/// A backend-neutral pane geometry mutation.
///
/// Backends that expose only one form must declare that explicitly in their capability descriptor;
/// callers never need to translate a resize into backend command-line syntax.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum MuxPaneResize {
    Directional {
        direction: MuxDirection,
        cells: u16,
    },
    Absolute {
        columns: Option<u16>,
        rows: Option<u16>,
    },
}

impl MuxPaneResize {
    pub fn is_valid(self) -> bool {
        match self {
            Self::Directional { cells, .. } => cells > 0,
            Self::Absolute { columns, rows } => {
                (columns.is_some() || rows.is_some())
                    && columns.is_none_or(|columns| columns > 0)
                    && rows.is_none_or(|rows| rows > 0)
            }
        }
    }
}

pub const MAX_LAUNCH_WINDOWS: usize = 64;
pub const MAX_LAUNCH_PANES: usize = 256;
pub const MAX_LAUNCH_DEPTH: usize = 32;
pub const MAX_LAUNCH_ARGUMENTS: usize = 128;
pub const MAX_LAUNCH_ENVIRONMENT_ENTRIES: usize = 128;
pub const MAX_LAUNCH_STRING_BYTES: usize = 16 * 1024;
pub const MAX_LAUNCH_CWD_BYTES: usize = 4 * 1024;
pub const MAX_LAUNCH_TITLE_BYTES: usize = 1024;
/// Maximum bytes occupied by a pane's environment and process intent after tmux's argv-to-shell
/// encoding and the remote shell's single-quote encoding.
///
/// This leaves room for the bounded session, window, and cwd arguments within the smallest
/// supported command-line limit while preventing one pane from creating a multi-megabyte command.
pub const MAX_LAUNCH_ENCODED_PANE_BYTES: usize = 64 * 1024;
/// Maximum JSON bytes in a normalized launch plan before it crosses a remote transport.
///
/// A base64url envelope for this payload remains below conservative OS argument-vector limits and
/// well below the remote rmux protocol's one-megabyte frame cap.
pub const MAX_LAUNCH_TRANSPORT_BYTES: usize = 64 * 1024;
pub const MIN_LAUNCH_RATIO_MILLIS: u16 = 50;
pub const MAX_LAUNCH_RATIO_MILLIS: u16 = 950;

/// Validation errors for a normalized launch plan at a backend or protocol boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MuxSessionLaunchPlanError {
    EmptyValue { field: &'static str },
    InvalidEnvironmentName { name: String },
    InvalidArgumentVector,
    ConflictingCommand,
    InvalidRatio,
    ContainsNul { field: &'static str },
    LimitExceeded { field: &'static str, limit: usize },
    EncodedCommandTooLarge { limit: usize },
    TransportPayloadTooLarge { limit: usize },
    InvalidFocusedWindow,
}

impl fmt::Display for MuxSessionLaunchPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyValue { field } => {
                write!(formatter, "session launch {field} must not be empty")
            }
            Self::InvalidEnvironmentName { name } => {
                write!(
                    formatter,
                    "session launch environment name {name:?} is invalid"
                )
            }
            Self::InvalidArgumentVector => {
                formatter.write_str("session launch argv must include a non-empty program")
            }
            Self::ConflictingCommand => {
                formatter.write_str("session launch command and argv are mutually exclusive")
            }
            Self::InvalidRatio => {
                formatter.write_str("session launch split ratio must leave room for both children")
            }
            Self::ContainsNul { field } => {
                write!(
                    formatter,
                    "session launch {field} must not contain a NUL byte"
                )
            }
            Self::LimitExceeded { field, limit } => {
                write!(
                    formatter,
                    "session launch {field} exceeds its limit of {limit}"
                )
            }
            Self::EncodedCommandTooLarge { limit } => {
                write!(
                    formatter,
                    "session launch encoded pane command exceeds its limit of {limit}"
                )
            }
            Self::TransportPayloadTooLarge { limit } => {
                write!(
                    formatter,
                    "session launch transport payload exceeds its limit of {limit}"
                )
            }
            Self::InvalidFocusedWindow => {
                formatter.write_str("session launch focused window is outside the launch plan")
            }
        }
    }
}

impl std::error::Error for MuxSessionLaunchPlanError {}

/// A fully normalized recursive session launch plan. This deliberately lives in the mux crate:
/// application descriptors normalize into it, while every backend consumes the same immutable
/// backend-neutral intent.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MuxSessionLaunchPlan {
    pub session_id: String,
    pub focus: bool,
    pub default_cwd: String,
    pub environment: BTreeMap<String, String>,
    pub windows: Vec<MuxWindowLaunchPlan>,
    pub focused_window: usize,
}

impl MuxSessionLaunchPlan {
    pub fn pane_count(&self) -> usize {
        self.windows
            .iter()
            .map(|window| window.layout.pane_count())
            .sum()
    }

    pub fn has_non_default_split_ratio(&self) -> bool {
        self.windows
            .iter()
            .any(|window| window.layout.has_non_default_split_ratio())
    }

    /// Revalidates immutable launch intent wherever it crosses into a backend or remote protocol.
    pub fn validate(&self) -> Result<(), MuxSessionLaunchPlanError> {
        validate_launch_string(
            &self.session_id,
            "session_id",
            MAX_LAUNCH_STRING_BYTES,
            true,
        )?;
        validate_launch_string(&self.default_cwd, "default_cwd", MAX_LAUNCH_CWD_BYTES, true)?;
        validate_environment(&self.environment, "session environment")?;
        if self.windows.is_empty() {
            return Err(MuxSessionLaunchPlanError::EmptyValue { field: "windows" });
        }
        if self.windows.len() > MAX_LAUNCH_WINDOWS {
            return Err(MuxSessionLaunchPlanError::LimitExceeded {
                field: "windows",
                limit: MAX_LAUNCH_WINDOWS,
            });
        }
        if self.focused_window >= self.windows.len() {
            return Err(MuxSessionLaunchPlanError::InvalidFocusedWindow);
        }

        let mut panes = 0;
        for window in &self.windows {
            if let Some(name) = &window.name {
                validate_launch_string(name, "window name", MAX_LAUNCH_STRING_BYTES, true)?;
            }
            validate_launch_layout(&window.layout, &self.environment, 1, &mut panes)?;
        }
        validate_launch_transport_size(self)?;
        Ok(())
    }
}

fn validate_launch_layout(
    layout: &MuxPaneLaunchPlan,
    inherited_environment: &BTreeMap<String, String>,
    depth: usize,
    panes: &mut usize,
) -> Result<(), MuxSessionLaunchPlanError> {
    if depth > MAX_LAUNCH_DEPTH {
        return Err(MuxSessionLaunchPlanError::LimitExceeded {
            field: "split depth",
            limit: MAX_LAUNCH_DEPTH,
        });
    }
    match layout {
        MuxPaneLaunchPlan::Pane(pane) => {
            *panes = panes.saturating_add(1);
            if *panes > MAX_LAUNCH_PANES {
                return Err(MuxSessionLaunchPlanError::LimitExceeded {
                    field: "panes",
                    limit: MAX_LAUNCH_PANES,
                });
            }
            validate_launch_string(&pane.cwd, "pane cwd", MAX_LAUNCH_CWD_BYTES, true)?;
            validate_environment(&pane.environment, "pane environment")?;
            if pane.command.is_some() && pane.argv.is_some() {
                return Err(MuxSessionLaunchPlanError::ConflictingCommand);
            }
            if let Some(command) = &pane.command {
                validate_launch_string(command, "pane command", MAX_LAUNCH_STRING_BYTES, true)?;
            }
            if let Some(argv) = &pane.argv {
                if argv.is_empty() || argv.first().is_none_or(String::is_empty) {
                    return Err(MuxSessionLaunchPlanError::InvalidArgumentVector);
                }
                if argv.len() > MAX_LAUNCH_ARGUMENTS {
                    return Err(MuxSessionLaunchPlanError::LimitExceeded {
                        field: "argv",
                        limit: MAX_LAUNCH_ARGUMENTS,
                    });
                }
                for argument in argv {
                    validate_launch_string(argument, "argv", MAX_LAUNCH_STRING_BYTES, false)?;
                }
            }
            if let Some(title) = &pane.title {
                validate_launch_string(title, "pane title", MAX_LAUNCH_TITLE_BYTES, false)?;
            }
            let effective_environment = pane.effective_environment(inherited_environment);
            if effective_environment.count() > MAX_LAUNCH_ENVIRONMENT_ENTRIES {
                return Err(MuxSessionLaunchPlanError::LimitExceeded {
                    field: "effective environment",
                    limit: MAX_LAUNCH_ENVIRONMENT_ENTRIES,
                });
            }
            validate_encoded_launch_pane_size(
                pane.command.as_deref(),
                pane.argv.as_deref(),
                pane.effective_environment(inherited_environment),
            )?;
            Ok(())
        }
        MuxPaneLaunchPlan::Split(split) => {
            if !(MIN_LAUNCH_RATIO_MILLIS..=MAX_LAUNCH_RATIO_MILLIS).contains(&split.ratio_millis) {
                return Err(MuxSessionLaunchPlanError::InvalidRatio);
            }
            validate_launch_layout(
                &split.first,
                inherited_environment,
                depth.saturating_add(1),
                panes,
            )?;
            validate_launch_layout(
                &split.second,
                inherited_environment,
                depth.saturating_add(1),
                panes,
            )
        }
    }
}

fn validate_environment(
    environment: &BTreeMap<String, String>,
    field: &'static str,
) -> Result<(), MuxSessionLaunchPlanError> {
    if environment.len() > MAX_LAUNCH_ENVIRONMENT_ENTRIES {
        return Err(MuxSessionLaunchPlanError::LimitExceeded {
            field,
            limit: MAX_LAUNCH_ENVIRONMENT_ENTRIES,
        });
    }
    for (name, value) in environment {
        if name.is_empty() || name.contains('=') {
            return Err(MuxSessionLaunchPlanError::InvalidEnvironmentName { name: name.clone() });
        }
        validate_launch_string(name, "environment name", MAX_LAUNCH_STRING_BYTES, false)?;
        validate_launch_string(value, field, MAX_LAUNCH_STRING_BYTES, false)?;
    }
    Ok(())
}

fn validate_launch_string(
    value: &str,
    field: &'static str,
    limit: usize,
    required: bool,
) -> Result<(), MuxSessionLaunchPlanError> {
    if value.contains('\0') {
        return Err(MuxSessionLaunchPlanError::ContainsNul { field });
    }
    if value.len() > limit {
        return Err(MuxSessionLaunchPlanError::LimitExceeded { field, limit });
    }
    if required && value.is_empty() {
        return Err(MuxSessionLaunchPlanError::EmptyValue { field });
    }
    Ok(())
}

/// Counts the variable portion of a tmux launch command after every supported shell encoding.
///
/// The result includes pane `-e NAME=VALUE` arguments and either the command text or tmux's
/// shell-quoted `argv` adapter. `None` represents arithmetic overflow.
pub fn encoded_launch_pane_bytes(
    command: Option<&str>,
    argv: Option<&[String]>,
    environment: &BTreeMap<String, String>,
) -> Option<usize> {
    encoded_launch_pane_bytes_for_environment(
        command,
        argv,
        environment
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    )
}

fn validate_encoded_launch_pane_size<'a>(
    command: Option<&str>,
    argv: Option<&[String]>,
    environment: impl Iterator<Item = (&'a str, &'a str)>,
) -> Result<(), MuxSessionLaunchPlanError> {
    match encoded_launch_pane_bytes_for_environment(command, argv, environment) {
        Some(bytes) if bytes <= MAX_LAUNCH_ENCODED_PANE_BYTES => Ok(()),
        Some(_) | None => Err(MuxSessionLaunchPlanError::EncodedCommandTooLarge {
            limit: MAX_LAUNCH_ENCODED_PANE_BYTES,
        }),
    }
}

fn encoded_launch_pane_bytes_for_environment<'a>(
    command: Option<&str>,
    argv: Option<&[String]>,
    environment: impl Iterator<Item = (&'a str, &'a str)>,
) -> Option<usize> {
    let mut bytes = 0;
    for (name, value) in environment {
        add_shell_quoted_argument_bytes(&mut bytes, "-e")?;
        add_shell_quoted_name_value_bytes(&mut bytes, name, value)?;
    }
    match (command, argv) {
        (Some(command), None) => add_shell_quoted_argument_bytes(&mut bytes, command)?,
        (None, Some(argv)) => add_shell_quoted_tmux_argv_bytes(&mut bytes, argv)?,
        (None, None) => {}
        (Some(_), Some(_)) => return None,
    }
    Some(bytes)
}

fn add_shell_quoted_argument_bytes(bytes: &mut usize, value: &str) -> Option<()> {
    let quote_count = value.bytes().filter(|byte| *byte == b'\'').count();
    add_shell_quoted_bytes(bytes, value.len(), quote_count)
}

fn add_shell_quoted_name_value_bytes(bytes: &mut usize, name: &str, value: &str) -> Option<()> {
    let length = name.len().checked_add(1)?.checked_add(value.len())?;
    let quote_count = name
        .bytes()
        .chain(value.bytes())
        .filter(|byte| *byte == b'\'')
        .count();
    add_shell_quoted_bytes(bytes, length, quote_count)
}

fn add_shell_quoted_tmux_argv_bytes(bytes: &mut usize, argv: &[String]) -> Option<()> {
    let mut tmux_command_bytes = 4usize;
    let mut tmux_quote_count = 0usize;
    for argument in argv {
        let quote_count = argument.bytes().filter(|byte| *byte == b'\'').count();
        tmux_command_bytes = tmux_command_bytes
            .checked_add(3)?
            .checked_add(argument.len())?
            .checked_add(quote_count.checked_mul(4)?)?;
        tmux_quote_count = tmux_quote_count
            .checked_add(2)?
            .checked_add(quote_count.checked_mul(3)?)?;
    }
    add_shell_quoted_bytes(bytes, tmux_command_bytes, tmux_quote_count)
}

fn add_shell_quoted_bytes(bytes: &mut usize, length: usize, quote_count: usize) -> Option<()> {
    let quoted = length
        .checked_add(quote_count.checked_mul(3)?)?
        .checked_add(2)?;
    *bytes = bytes.checked_add(1)?.checked_add(quoted)?;
    Some(())
}

fn validate_launch_transport_size(
    plan: &MuxSessionLaunchPlan,
) -> Result<(), MuxSessionLaunchPlanError> {
    let mut counter = LaunchByteCounter::default();
    serde_json::to_writer(&mut counter, plan).map_err(|_| {
        MuxSessionLaunchPlanError::TransportPayloadTooLarge {
            limit: MAX_LAUNCH_TRANSPORT_BYTES,
        }
    })?;
    Ok(())
}

#[derive(Default)]
struct LaunchByteCounter {
    bytes: usize,
}

impl Write for LaunchByteCounter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(buffer.len())
            .filter(|bytes| *bytes <= MAX_LAUNCH_TRANSPORT_BYTES)
            .ok_or_else(|| std::io::Error::other("launch payload byte count exceeds its limit"))?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MuxWindowLaunchPlan {
    pub name: Option<String>,
    pub focus: bool,
    pub layout: MuxPaneLaunchPlan,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MuxPaneLaunchPlan {
    Pane(MuxPaneLaunch),
    Split(MuxSplitLaunch),
}

impl MuxPaneLaunchPlan {
    pub fn pane_count(&self) -> usize {
        match self {
            Self::Pane(_) => 1,
            Self::Split(split) => split.first.pane_count() + split.second.pane_count(),
        }
    }

    pub fn has_non_default_split_ratio(&self) -> bool {
        match self {
            Self::Pane(_) => false,
            Self::Split(split) => {
                split.ratio_millis != 500
                    || split.first.has_non_default_split_ratio()
                    || split.second.has_non_default_split_ratio()
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MuxPaneLaunch {
    pub cwd: String,
    /// Normative shell command. It is mutually exclusive with the compatibility argv form.
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub argv: Option<Vec<String>>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub title: Option<String>,
}

/// An allocation-free, deterministic overlay of session and pane launch environment maps.
///
/// Keys are yielded in lexical order. A pane value replaces an inherited value with the same key.
pub(crate) struct MuxPaneEffectiveEnvironment<'a> {
    inherited: std::iter::Peekable<std::collections::btree_map::Iter<'a, String, String>>,
    overrides: std::iter::Peekable<std::collections::btree_map::Iter<'a, String, String>>,
}

impl<'a> MuxPaneEffectiveEnvironment<'a> {
    fn new(
        inherited: &'a BTreeMap<String, String>,
        overrides: &'a BTreeMap<String, String>,
    ) -> Self {
        Self {
            inherited: inherited.iter().peekable(),
            overrides: overrides.iter().peekable(),
        }
    }

    fn next_inherited(&mut self) -> Option<(&'a str, &'a str)> {
        self.inherited
            .next()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }

    fn next_override(&mut self) -> Option<(&'a str, &'a str)> {
        self.overrides
            .next()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }
}

impl<'a> Iterator for MuxPaneEffectiveEnvironment<'a> {
    type Item = (&'a str, &'a str);

    fn next(&mut self) -> Option<Self::Item> {
        match (
            self.inherited.peek().map(|(name, _)| *name),
            self.overrides.peek().map(|(name, _)| *name),
        ) {
            (Some(inherited_name), Some(override_name)) => {
                match inherited_name.cmp(override_name) {
                    std::cmp::Ordering::Less => self.next_inherited(),
                    std::cmp::Ordering::Equal => {
                        self.inherited.next();
                        self.next_override()
                    }
                    std::cmp::Ordering::Greater => self.next_override(),
                }
            }
            (Some(_), None) => self.next_inherited(),
            (None, Some(_)) => self.next_override(),
            (None, None) => None,
        }
    }
}

impl MuxPaneLaunch {
    /// Iterates every environment entry visible to this pane without mutating either descriptor.
    pub(crate) fn effective_environment<'a>(
        &'a self,
        inherited: &'a BTreeMap<String, String>,
    ) -> MuxPaneEffectiveEnvironment<'a> {
        MuxPaneEffectiveEnvironment::new(inherited, &self.environment)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MuxSplitLaunch {
    pub direction: MuxSplitDirection,
    /// The portion of the available axis owned by the first child, in thousandths.
    pub ratio_millis: u16,
    pub first: Box<MuxPaneLaunchPlan>,
    pub second: Box<MuxPaneLaunchPlan>,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum MuxCommand {
    ActivateWindow {
        session_id: String,
        window_id: String,
    },
    NewWindow {
        session_id: String,
        cwd: Option<String>,
    },
    RenameWindow {
        session_id: String,
        window_id: String,
        name: String,
    },
    ActivateNextWindow {
        session_id: String,
    },
    ActivatePreviousWindow {
        session_id: String,
    },
    ActivateLastWindow {
        session_id: String,
    },
    ActivateWindowIndex {
        session_id: String,
        index: u32,
    },
    MoveWindow {
        session_id: String,
        window_id: Option<String>,
        delta: i32,
    },
    /// Reorder a specific window, then restore the window that was active before the move.
    /// Context-menu moves use this so moving an inactive tab does not steal focus.
    MoveWindowPreservingSelection {
        session_id: String,
        window_id: String,
        delta: i32,
        selected_window_id: String,
    },
    SplitPane {
        session_id: String,
        /// The pane to split (its cwd seeds the new pane). `None` splits the window's active pane.
        pane_id: Option<String>,
        direction: MuxSplitDirection,
    },
    SelectPane {
        session_id: String,
        /// The window whose pane selection should move. `None` uses the session's active window.
        window_id: Option<String>,
        direction: MuxDirection,
    },
    SelectNextPane {
        session_id: String,
        window_id: Option<String>,
    },
    SelectPreviousPane {
        session_id: String,
        window_id: Option<String>,
    },
    /// Restores focus to the window's most recently active pane.
    SelectLastPane {
        session_id: String,
        /// The window whose previous pane should be restored. `None` uses the session's active
        /// window.
        window_id: Option<String>,
    },
    KillPane {
        session_id: String,
        /// The pane to remove. `None` targets the window's active pane.
        pane_id: Option<String>,
    },
    // Close the active pane and cascade: an emptied window (tab) is removed; a session whose last
    // window is removed is left empty rather than deleted.
    ClosePane {
        session_id: String,
        /// The pane to close. `None` targets the window's active pane.
        pane_id: Option<String>,
    },
    TogglePaneZoom {
        session_id: String,
        /// The pane to zoom. `None` targets the window's active pane.
        pane_id: Option<String>,
    },
    /// Applies a backend-neutral relative or absolute pane geometry change.
    ResizePane {
        session_id: String,
        /// The pane to resize. `None` targets the window's active pane.
        pane_id: Option<String>,
        adjustment: MuxPaneResize,
    },
    /// Creates a session from a fully normalized recursive launch plan.
    CreateSession {
        plan: MuxSessionLaunchPlan,
    },
    CreateProjectSession {
        session_id: String,
        cwd: String,
    },
    CreateWorktreeSession {
        session_id: String,
        cwd: String,
    },
    RenameSession {
        session_id: String,
        name: String,
    },
    DitchSession {
        session_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_with_argv(argv: Vec<String>) -> MuxSessionLaunchPlan {
        MuxSessionLaunchPlan {
            session_id: "launch-bound".to_owned(),
            focus: true,
            default_cwd: "/repo".to_owned(),
            environment: BTreeMap::new(),
            windows: vec![MuxWindowLaunchPlan {
                name: None,
                focus: true,
                layout: MuxPaneLaunchPlan::Pane(MuxPaneLaunch {
                    cwd: "/repo".to_owned(),
                    command: None,
                    argv: Some(argv),
                    environment: BTreeMap::new(),
                    title: None,
                }),
            }],
            focused_window: 0,
        }
    }

    #[test]
    fn encoded_pane_limit_counts_tmux_and_remote_shell_quote_expansion() {
        // One quote expands through tmux's argv adapter and then through the remote shell:
        // 16 bytes of fixed syntax plus 14 bytes per quote.
        let quote_count = (MAX_LAUNCH_ENCODED_PANE_BYTES - 16) / 14;
        let argv = vec!["'".repeat(quote_count)];

        assert_eq!(
            encoded_launch_pane_bytes(None, Some(&argv), &BTreeMap::new()),
            Some(16 + 14 * quote_count)
        );
        assert!(plan_with_argv(argv).validate().is_ok());

        assert_eq!(
            plan_with_argv(vec!["'".repeat(quote_count + 1)]).validate(),
            Err(MuxSessionLaunchPlanError::EncodedCommandTooLarge {
                limit: MAX_LAUNCH_ENCODED_PANE_BYTES,
            })
        );
    }

    #[test]
    fn launch_plan_transport_limit_rejects_combined_pane_payloads() {
        let command = "x".repeat(MAX_LAUNCH_STRING_BYTES);
        let plan = MuxSessionLaunchPlan {
            session_id: "launch-transport-bound".to_owned(),
            focus: true,
            default_cwd: "/repo".to_owned(),
            environment: BTreeMap::new(),
            windows: (0..4)
                .map(|_| MuxWindowLaunchPlan {
                    name: None,
                    focus: false,
                    layout: MuxPaneLaunchPlan::Pane(MuxPaneLaunch {
                        cwd: "/repo".to_owned(),
                        command: Some(command.clone()),
                        argv: None,
                        environment: BTreeMap::new(),
                        title: None,
                    }),
                })
                .collect(),
            focused_window: 0,
        };

        assert_eq!(
            plan.validate(),
            Err(MuxSessionLaunchPlanError::TransportPayloadTooLarge {
                limit: MAX_LAUNCH_TRANSPORT_BYTES,
            })
        );
    }

    #[test]
    fn inherited_environment_is_included_in_each_pane_command_bound() {
        let plan = MuxSessionLaunchPlan {
            session_id: "effective-environment-bound".to_owned(),
            focus: true,
            default_cwd: "/repo".to_owned(),
            environment: (0..4)
                .map(|index| (format!("KEY_{index}"), "x".repeat(MAX_LAUNCH_STRING_BYTES)))
                .collect(),
            windows: vec![MuxWindowLaunchPlan {
                name: None,
                focus: true,
                layout: MuxPaneLaunchPlan::Pane(MuxPaneLaunch {
                    cwd: "/repo".to_owned(),
                    command: None,
                    argv: None,
                    environment: BTreeMap::new(),
                    title: None,
                }),
            }],
            focused_window: 0,
        };

        assert_eq!(
            plan.validate(),
            Err(MuxSessionLaunchPlanError::EncodedCommandTooLarge {
                limit: MAX_LAUNCH_ENCODED_PANE_BYTES,
            })
        );
    }
}

#[cfg(feature = "app")]
impl MuxCommand {
    pub fn operation(&self) -> BindingOperation {
        match self {
            Self::ActivateWindow { .. } => BindingOperation::ActivateWindow,
            Self::NewWindow { .. } => BindingOperation::CreateWindow,
            Self::RenameWindow { .. } => BindingOperation::RenameWindow,
            Self::ActivateNextWindow { .. }
            | Self::ActivatePreviousWindow { .. }
            | Self::ActivateLastWindow { .. }
            | Self::ActivateWindowIndex { .. } => BindingOperation::NavigateWindow,
            Self::MoveWindow { .. } | Self::MoveWindowPreservingSelection { .. } => {
                BindingOperation::MoveWindow
            }
            Self::SplitPane { .. } => BindingOperation::SplitPane,
            Self::SelectPane { .. }
            | Self::SelectNextPane { .. }
            | Self::SelectPreviousPane { .. } => BindingOperation::NavigatePane,
            Self::SelectLastPane { .. } => BindingOperation::LastPane,
            Self::KillPane { .. } | Self::ClosePane { .. } => BindingOperation::ClosePane,
            Self::TogglePaneZoom { .. } => BindingOperation::TogglePaneZoom,
            Self::ResizePane { .. } => BindingOperation::ResizePane,
            Self::CreateSession { .. } | Self::CreateProjectSession { .. } => {
                BindingOperation::CreateProjectSession
            }
            Self::CreateWorktreeSession { .. } => BindingOperation::CreateWorktreeSession,
            Self::RenameSession { .. } => BindingOperation::RenameSession,
            Self::DitchSession { .. } => BindingOperation::DitchSession,
        }
    }
}

#[cfg(test)]
mod effective_environment_tests {
    use std::collections::BTreeMap;

    use super::MuxPaneLaunch;

    #[test]
    fn effective_environment_inherits_overrides_and_preserves_both_descriptors() {
        let inherited = BTreeMap::from([
            ("A".to_owned(), "session-a".to_owned()),
            ("OVERRIDE".to_owned(), "session".to_owned()),
            ("Z".to_owned(), "session-z".to_owned()),
        ]);
        let pane = MuxPaneLaunch {
            cwd: "/repo".to_owned(),
            command: None,
            argv: None,
            environment: BTreeMap::from([
                ("B".to_owned(), "pane-b".to_owned()),
                ("OVERRIDE".to_owned(), "pane".to_owned()),
                ("Y".to_owned(), "pane-y".to_owned()),
            ]),
            title: None,
        };
        let inherited_before = inherited.clone();
        let pane_environment_before = pane.environment.clone();

        assert_eq!(
            pane.effective_environment(&inherited).collect::<Vec<_>>(),
            vec![
                ("A", "session-a"),
                ("B", "pane-b"),
                ("OVERRIDE", "pane"),
                ("Y", "pane-y"),
                ("Z", "session-z"),
            ]
        );
        assert_eq!(inherited, inherited_before);
        assert_eq!(pane.environment, pane_environment_before);
    }
}
