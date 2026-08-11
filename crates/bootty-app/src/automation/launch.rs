use std::{
    collections::{BTreeMap, BTreeSet},
    env, fmt,
    path::Path,
};

use serde::{Deserialize, Serialize};

use crate::{
    automation::directory::DirectoryRef,
    mux::command::{
        MAX_LAUNCH_ENCODED_PANE_BYTES, MuxPaneLaunch, MuxPaneLaunchPlan, MuxSessionLaunchPlan,
        MuxSplitDirection, MuxSplitLaunch, MuxWindowLaunchPlan, encoded_launch_pane_bytes,
    },
};

/// The largest launch tree accepted in one atomic session-create request.
pub const MAX_LAUNCH_WINDOWS: usize = 64;
/// The largest number of panes across one launch tree.
pub const MAX_LAUNCH_PANES: usize = 256;
/// The deepest split tree accepted for a single window.
pub const MAX_LAUNCH_DEPTH: usize = 32;
/// The largest literal argv accepted for a pane process.
pub const MAX_LAUNCH_ARGUMENTS: usize = 128;
/// The largest effective environment accepted for a pane process.
pub const MAX_LAUNCH_ENVIRONMENT_ENTRIES: usize = 128;
/// A launch string must fit within this bound before it reaches a backend protocol.
pub const MAX_LAUNCH_STRING_BYTES: usize = 16 * 1024;
/// Working directories need a tighter bound because all supported backends carry them in one field.
pub const MAX_LAUNCH_CWD_BYTES: usize = 4 * 1024;
/// Pane titles are UX labels, not arbitrary payloads.
pub const MAX_LAUNCH_TITLE_BYTES: usize = 1024;
/// A split must leave a usable region for both children.
pub const MIN_LAUNCH_RATIO_MILLIS: u16 = 50;
/// A split must leave a usable region for both children.
pub const MAX_LAUNCH_RATIO_MILLIS: u16 = 950;
/// The deterministic ratio used when a split does not specify one.
pub const DEFAULT_LAUNCH_RATIO_MILLIS: u16 = 500;

/// Mutable request data accepted by `session.create` before normalization.
///
/// `windows: None` means one default window with one default pane. An explicit empty window list is
/// rejected so every successful descriptor has a concrete terminal to create.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionLaunchDescriptor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<Vec<WindowLaunchDescriptor>>,
}

impl SessionLaunchDescriptor {
    /// The descriptor equivalent of the existing one-window, one-pane UI action.
    #[must_use]
    pub fn simple(name: impl Into<String>, cwd: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            focus: Some(true),
            default_cwd: Some(cwd.into()),
            environment: None,
            windows: None,
        }
    }

    /// Normalizes a local launch against the process home directory only when no `default_cwd`
    /// was supplied.
    pub fn normalize(&self) -> Result<NormalizedSessionLaunch, LaunchValidationError> {
        let default_cwd = match &self.default_cwd {
            Some(default_cwd) => default_cwd.clone(),
            None => launch_home_directory()?,
        };
        self.normalize_with_resolved_default_cwd(default_cwd, LaunchCwdScope::Local)
    }

    /// Normalizes a local launch against an explicitly supplied default directory. Local
    /// directories are canonicalized through `DirectoryRef` before they reach a backend.
    pub fn normalize_with_default_cwd(
        &self,
        fallback_cwd: impl Into<String>,
    ) -> Result<NormalizedSessionLaunch, LaunchValidationError> {
        let fallback_cwd = fallback_cwd.into();
        let default_cwd = self.default_cwd.clone().unwrap_or(fallback_cwd);
        self.normalize_with_resolved_default_cwd(default_cwd, LaunchCwdScope::Local)
    }

    /// Normalizes a remote launch without consulting the local process environment or filesystem.
    ///
    /// Remote backend homes are not interchangeable with the local `$HOME`, so callers must
    /// resolve an authoritative remote home themselves or provide `default_cwd`.
    pub fn normalize_for_remote(&self) -> Result<NormalizedSessionLaunch, LaunchValidationError> {
        let default_cwd = self
            .default_cwd
            .clone()
            .ok_or(LaunchValidationError::RemoteDefaultCwdRequired)?;
        self.normalize_with_resolved_default_cwd(default_cwd, LaunchCwdScope::Remote)
    }

    fn normalize_with_resolved_default_cwd(
        &self,
        default_cwd: String,
        scope: LaunchCwdScope,
    ) -> Result<NormalizedSessionLaunch, LaunchValidationError> {
        validate_optional_name(self.name.as_deref(), "session name")?;
        let default_cwd = normalize_cwd(&default_cwd, "session default_cwd", scope)?;

        let session_environment = normalize_environment(
            self.environment.as_ref(),
            "session environment",
            &BTreeMap::new(),
        )?;
        let windows = self.windows.clone().unwrap_or_else(|| {
            vec![WindowLaunchDescriptor {
                name: None,
                focus: None,
                layout: PaneLaunchDescriptor::Pane(PaneLaunch::default()),
            }]
        });
        if windows.is_empty() {
            return Err(LaunchValidationError::EmptyWindows);
        }
        if windows.len() > MAX_LAUNCH_WINDOWS {
            return Err(LaunchValidationError::LimitExceeded {
                field: "windows",
                limit: MAX_LAUNCH_WINDOWS,
            });
        }

        let focused_windows = windows
            .iter()
            .enumerate()
            .filter_map(|(index, window)| (window.focus == Some(true)).then_some(index))
            .collect::<Vec<_>>();
        if focused_windows.len() > 1 {
            return Err(LaunchValidationError::AmbiguousFocus { scope: "windows" });
        }
        if self.focus == Some(false) && !focused_windows.is_empty() {
            return Err(LaunchValidationError::ConflictingFocus);
        }

        let mut window_names = BTreeSet::new();
        let mut pane_count = 0usize;
        let mut normalized_windows = Vec::with_capacity(windows.len());
        for window in windows {
            validate_optional_name(window.name.as_deref(), "window name")?;
            if let Some(name) = &window.name
                && !window_names.insert(name.clone())
            {
                return Err(LaunchValidationError::DuplicateWindowName { name: name.clone() });
            }
            let layout = normalize_layout(
                &window.layout,
                &default_cwd,
                &session_environment,
                scope,
                1,
                &mut pane_count,
            )?;
            normalized_windows.push(NormalizedWindowLaunch {
                name: window.name,
                focus: window.focus == Some(true),
                layout,
            });

            if pane_count > MAX_LAUNCH_PANES {
                return Err(LaunchValidationError::LimitExceeded {
                    field: "panes",
                    limit: MAX_LAUNCH_PANES,
                });
            }
        }

        let focused_window = focused_windows
            .first()
            .copied()
            .or_else(|| normalized_windows.iter().position(|window| !window.focus))
            .unwrap_or(0);
        if let Some(window) = normalized_windows.get_mut(focused_window) {
            window.focus = true;
        }

        Ok(NormalizedSessionLaunch {
            name: self.name.clone(),
            focus: self.focus.unwrap_or(true),
            default_cwd: default_cwd.cwd,
            default_local_directory: default_cwd.local_directory,
            environment: session_environment,
            windows: normalized_windows,
            focused_window,
        })
    }
}

/// One tab/window within a session launch descriptor.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WindowLaunchDescriptor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<bool>,
    pub layout: PaneLaunchDescriptor,
}

/// A recursively nested pane layout.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PaneLaunchDescriptor {
    Pane(PaneLaunch),
    Split(SplitLaunch),
}

impl Default for PaneLaunchDescriptor {
    fn default() -> Self {
        Self::Pane(PaneLaunch::default())
    }
}

/// Leaf process intent for one pane.
///
/// `command` is the normative shell command text. It is handed to a backend unchanged, so callers
/// own its shell syntax and quoting. `argv` is the compatible structured input; a descriptor must
/// provide at most one of them.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PaneLaunch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Compatible structured process input. New descriptors should use `command`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argv: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// An internal node that splits one region into two recursively launched children.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SplitLaunch {
    pub direction: LaunchSplitDirection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f64>,
    pub first: Box<PaneLaunchDescriptor>,
    pub second: Box<PaneLaunchDescriptor>,
}

/// Backend-neutral split direction. `Right` creates a left/right region; `Down` creates a top/bottom
/// region.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchSplitDirection {
    Right,
    Down,
}

impl From<LaunchSplitDirection> for MuxSplitDirection {
    fn from(direction: LaunchSplitDirection) -> Self {
        match direction {
            LaunchSplitDirection::Right => Self::Right,
            LaunchSplitDirection::Down => Self::Down,
        }
    }
}

/// Validation errors produced before any backend mutation begins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchValidationError {
    HomeDirectoryUnavailable,
    RemoteDefaultCwdRequired,
    LocalDirectoryUnavailable { field: &'static str },
    LocalDirectoryNotUtf8 { field: &'static str },
    EmptyWindows,
    AmbiguousFocus { scope: &'static str },
    ConflictingFocus,
    DuplicateWindowName { name: String },
    EmptyValue { field: &'static str },
    InvalidEnvironmentName { name: String },
    InvalidArgumentVector,
    CommandAndArgv,
    InvalidRatio,
    ContainsNul { field: &'static str },
    EncodedCommandTooLarge { limit: usize },
    LimitExceeded { field: &'static str, limit: usize },
}

impl fmt::Display for LaunchValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HomeDirectoryUnavailable => {
                formatter.write_str("session launch requires a local $HOME")
            }
            Self::RemoteDefaultCwdRequired => {
                formatter.write_str("remote session launch requires an explicit default_cwd")
            }
            Self::LocalDirectoryUnavailable { field } => {
                write!(
                    formatter,
                    "session launch {field} could not be resolved locally"
                )
            }
            Self::LocalDirectoryNotUtf8 { field } => {
                write!(
                    formatter,
                    "session launch {field} is not valid UTF-8 after canonicalization"
                )
            }
            Self::EmptyWindows => {
                formatter.write_str("session launch requires at least one window")
            }
            Self::AmbiguousFocus { scope } => {
                write!(
                    formatter,
                    "session launch has more than one focused {scope}"
                )
            }
            Self::ConflictingFocus => {
                formatter.write_str("a session that declines focus cannot request a focused window")
            }
            Self::DuplicateWindowName { name } => {
                write!(
                    formatter,
                    "session launch names more than one window {name:?}"
                )
            }
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
            Self::CommandAndArgv => {
                formatter.write_str("session launch pane command and argv are mutually exclusive")
            }
            Self::InvalidRatio => formatter.write_str(
                "session launch split ratio must be finite and leave room for both children",
            ),
            Self::ContainsNul { field } => {
                write!(
                    formatter,
                    "session launch {field} must not contain a NUL byte"
                )
            }
            Self::EncodedCommandTooLarge { limit } => {
                write!(
                    formatter,
                    "session launch encoded pane command exceeds its limit of {limit}"
                )
            }
            Self::LimitExceeded { field, limit } => {
                write!(
                    formatter,
                    "session launch {field} exceeds its limit of {limit}"
                )
            }
        }
    }
}

impl std::error::Error for LaunchValidationError {}

/// Immutable normalized launch intent. All inherited values are already resolved; later terminal
/// cwd observations must never mutate these values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NormalizedSessionLaunch {
    name: Option<String>,
    focus: bool,
    default_cwd: String,
    #[serde(skip)]
    default_local_directory: Option<DirectoryRef>,
    environment: BTreeMap<String, String>,
    windows: Vec<NormalizedWindowLaunch>,
    focused_window: usize,
}

impl NormalizedSessionLaunch {
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    #[must_use]
    pub const fn focus(&self) -> bool {
        self.focus
    }

    #[must_use]
    pub fn default_cwd(&self) -> &str {
        &self.default_cwd
    }

    /// The typed local-directory identity used to canonicalize `default_cwd`, when this is a
    /// local launch. Remote paths deliberately have no local filesystem identity.
    #[must_use]
    pub fn default_local_directory(&self) -> Option<&DirectoryRef> {
        self.default_local_directory.as_ref()
    }

    #[must_use]
    pub fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }

    #[must_use]
    pub fn windows(&self) -> &[NormalizedWindowLaunch] {
        &self.windows
    }

    #[must_use]
    pub const fn focused_window_index(&self) -> usize {
        self.focused_window
    }

    #[must_use]
    pub fn pane_count(&self) -> usize {
        self.windows
            .iter()
            .map(|window| window.layout.pane_count())
            .sum()
    }

    /// Yields leaf panes in window declaration order and recursive depth-first declaration order.
    /// This is aligned with authoritative `MuxAllocatedResources.windows[*].pane_ids`.
    pub fn panes_depth_first(&self) -> impl Iterator<Item = &NormalizedPane> + '_ {
        NormalizedPaneDepthFirst {
            pending: self
                .windows
                .iter()
                .rev()
                .map(|window| &window.layout)
                .collect(),
        }
    }

    /// Effective leaf working directories in the same order as `panes_depth_first`.
    pub fn leaf_cwds(&self) -> impl Iterator<Item = &str> + '_ {
        self.panes_depth_first().map(NormalizedPane::cwd)
    }

    /// Binds a backend session identity without changing the normalized descriptor. The resulting
    /// mux plan is the exact immutable intent every backend consumes.
    #[must_use]
    pub fn mux_plan(&self, session_id: impl Into<String>) -> MuxSessionLaunchPlan {
        MuxSessionLaunchPlan {
            session_id: session_id.into(),
            focus: self.focus,
            default_cwd: self.default_cwd.clone(),
            environment: self.environment.clone(),
            windows: self
                .windows
                .iter()
                .map(NormalizedWindowLaunch::mux_plan)
                .collect(),
            focused_window: self.focused_window,
        }
    }
}

/// A normalized window with one unambiguous active layout root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NormalizedWindowLaunch {
    name: Option<String>,
    focus: bool,
    layout: NormalizedPaneLaunch,
}

impl NormalizedWindowLaunch {
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    #[must_use]
    pub const fn focus(&self) -> bool {
        self.focus
    }

    #[must_use]
    pub fn layout(&self) -> &NormalizedPaneLaunch {
        &self.layout
    }

    fn mux_plan(&self) -> MuxWindowLaunchPlan {
        MuxWindowLaunchPlan {
            name: self.name.clone(),
            focus: self.focus,
            layout: self.layout.mux_plan(),
        }
    }
}

/// Immutable normalized pane topology.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NormalizedPaneLaunch {
    Pane(Box<NormalizedPane>),
    Split(NormalizedSplitLaunch),
}

impl NormalizedPaneLaunch {
    #[must_use]
    pub fn pane_count(&self) -> usize {
        match self {
            Self::Pane(_) => 1,
            Self::Split(split) => split.first.pane_count() + split.second.pane_count(),
        }
    }

    fn mux_plan(&self) -> MuxPaneLaunchPlan {
        match self {
            Self::Pane(pane) => MuxPaneLaunchPlan::Pane(pane.mux_plan()),
            Self::Split(split) => MuxPaneLaunchPlan::Split(split.mux_plan()),
        }
    }
}

/// Immutable leaf process intent with inherited cwd and environment already resolved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NormalizedPane {
    cwd: String,
    #[serde(skip)]
    local_directory: Option<DirectoryRef>,
    command: Option<String>,
    argv: Option<Vec<String>>,
    environment: BTreeMap<String, String>,
    title: Option<String>,
}

impl NormalizedPane {
    #[must_use]
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    /// The typed local-directory identity used to canonicalize `cwd`, when this is a local pane.
    #[must_use]
    pub fn local_directory(&self) -> Option<&DirectoryRef> {
        self.local_directory.as_ref()
    }

    /// Normative shell command text, preserved verbatim for backend execution.
    #[must_use]
    pub fn command(&self) -> Option<&str> {
        self.command.as_deref()
    }

    #[must_use]
    pub fn argv(&self) -> Option<&[String]> {
        self.argv.as_deref()
    }

    #[must_use]
    pub fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }

    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    fn mux_plan(&self) -> MuxPaneLaunch {
        MuxPaneLaunch {
            cwd: self.cwd.clone(),
            command: self.command.clone(),
            argv: self.argv.clone(),
            environment: self.environment.clone(),
            title: self.title.clone(),
        }
    }
}

/// Immutable normalized split geometry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NormalizedSplitLaunch {
    direction: LaunchSplitDirection,
    ratio_millis: u16,
    first: Box<NormalizedPaneLaunch>,
    second: Box<NormalizedPaneLaunch>,
}

impl NormalizedSplitLaunch {
    #[must_use]
    pub const fn direction(&self) -> LaunchSplitDirection {
        self.direction
    }

    #[must_use]
    pub const fn ratio_millis(&self) -> u16 {
        self.ratio_millis
    }

    #[must_use]
    pub fn first(&self) -> &NormalizedPaneLaunch {
        &self.first
    }

    #[must_use]
    pub fn second(&self) -> &NormalizedPaneLaunch {
        &self.second
    }

    fn mux_plan(&self) -> MuxSplitLaunch {
        MuxSplitLaunch {
            direction: self.direction.into(),
            ratio_millis: self.ratio_millis,
            first: Box::new(self.first.mux_plan()),
            second: Box::new(self.second.mux_plan()),
        }
    }
}

struct NormalizedPaneDepthFirst<'a> {
    pending: Vec<&'a NormalizedPaneLaunch>,
}

impl<'a> Iterator for NormalizedPaneDepthFirst<'a> {
    type Item = &'a NormalizedPane;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(layout) = self.pending.pop() {
            match layout {
                NormalizedPaneLaunch::Pane(pane) => return Some(pane),
                NormalizedPaneLaunch::Split(split) => {
                    self.pending.push(&split.second);
                    self.pending.push(&split.first);
                }
            }
        }
        None
    }
}

fn launch_home_directory() -> Result<String, LaunchValidationError> {
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .filter(|home| !home.is_empty())
        .ok_or(LaunchValidationError::HomeDirectoryUnavailable)?;
    home.into_string()
        .map_err(|_| LaunchValidationError::LocalDirectoryNotUtf8 {
            field: "session default_cwd",
        })
}

#[derive(Clone, Copy)]
enum LaunchCwdScope {
    Local,
    Remote,
}

#[derive(Clone)]
struct ResolvedLaunchCwd {
    cwd: String,
    local_directory: Option<DirectoryRef>,
}

fn normalize_cwd(
    cwd: &str,
    field: &'static str,
    scope: LaunchCwdScope,
) -> Result<ResolvedLaunchCwd, LaunchValidationError> {
    validate_cwd(cwd, field)?;
    match scope {
        LaunchCwdScope::Remote => Ok(ResolvedLaunchCwd {
            cwd: cwd.to_owned(),
            local_directory: None,
        }),
        LaunchCwdScope::Local => {
            let directory = DirectoryRef::resolve(cwd)
                .map_err(|_| LaunchValidationError::LocalDirectoryUnavailable { field })?;
            let cwd = directory
                .canonical_path
                .to_str()
                .map(str::to_owned)
                .ok_or(LaunchValidationError::LocalDirectoryNotUtf8 { field })?;
            validate_cwd(&cwd, field)?;
            Ok(ResolvedLaunchCwd {
                cwd,
                local_directory: Some(directory),
            })
        }
    }
}

fn resolve_pane_cwd(
    cwd: Option<&str>,
    inherited_cwd: &ResolvedLaunchCwd,
    scope: LaunchCwdScope,
) -> Result<ResolvedLaunchCwd, LaunchValidationError> {
    let Some(cwd) = cwd else {
        return Ok(inherited_cwd.clone());
    };
    validate_cwd(cwd, "pane cwd")?;
    let cwd = if Path::new(cwd).is_relative() {
        Path::new(&inherited_cwd.cwd)
            .join(cwd)
            .to_string_lossy()
            .into_owned()
    } else {
        cwd.to_owned()
    };
    normalize_cwd(&cwd, "pane cwd", scope)
}

fn normalize_layout(
    layout: &PaneLaunchDescriptor,
    default_cwd: &ResolvedLaunchCwd,
    inherited_environment: &BTreeMap<String, String>,
    scope: LaunchCwdScope,
    depth: usize,
    pane_count: &mut usize,
) -> Result<NormalizedPaneLaunch, LaunchValidationError> {
    if depth > MAX_LAUNCH_DEPTH {
        return Err(LaunchValidationError::LimitExceeded {
            field: "split depth",
            limit: MAX_LAUNCH_DEPTH,
        });
    }
    match layout {
        PaneLaunchDescriptor::Pane(pane) => {
            *pane_count = pane_count.saturating_add(1);
            if *pane_count > MAX_LAUNCH_PANES {
                return Err(LaunchValidationError::LimitExceeded {
                    field: "panes",
                    limit: MAX_LAUNCH_PANES,
                });
            }
            let cwd = resolve_pane_cwd(pane.cwd.as_deref(), default_cwd, scope)?;
            let environment = normalize_environment(
                pane.environment.as_ref(),
                "pane environment",
                inherited_environment,
            )?;
            if pane.command.is_some() && pane.argv.is_some() {
                return Err(LaunchValidationError::CommandAndArgv);
            }
            validate_optional_name(pane.command.as_deref(), "pane command")?;
            validate_argv(pane.argv.as_deref())?;
            validate_optional_string(pane.title.as_deref(), "pane title", MAX_LAUNCH_TITLE_BYTES)?;
            validate_encoded_launch_pane_size(
                pane.command.as_deref(),
                pane.argv.as_deref(),
                &environment,
            )?;
            Ok(NormalizedPaneLaunch::Pane(Box::new(NormalizedPane {
                cwd: cwd.cwd,
                local_directory: cwd.local_directory,
                command: pane.command.clone(),
                argv: pane.argv.clone(),
                environment,
                title: pane.title.clone(),
            })))
        }
        PaneLaunchDescriptor::Split(split) => {
            let ratio_millis = normalize_ratio(split.ratio)?;
            let first = normalize_layout(
                &split.first,
                default_cwd,
                inherited_environment,
                scope,
                depth.saturating_add(1),
                pane_count,
            )?;
            let second = normalize_layout(
                &split.second,
                default_cwd,
                inherited_environment,
                scope,
                depth.saturating_add(1),
                pane_count,
            )?;
            Ok(NormalizedPaneLaunch::Split(NormalizedSplitLaunch {
                direction: split.direction,
                ratio_millis,
                first: Box::new(first),
                second: Box::new(second),
            }))
        }
    }
}

fn normalize_ratio(ratio: Option<f64>) -> Result<u16, LaunchValidationError> {
    let Some(ratio) = ratio else {
        return Ok(DEFAULT_LAUNCH_RATIO_MILLIS);
    };
    if !ratio.is_finite() {
        return Err(LaunchValidationError::InvalidRatio);
    }
    let ratio_millis = (ratio * f64::from(DEFAULT_LAUNCH_RATIO_MILLIS) * 2.0).round();
    if !(f64::from(MIN_LAUNCH_RATIO_MILLIS)..=f64::from(MAX_LAUNCH_RATIO_MILLIS))
        .contains(&ratio_millis)
    {
        return Err(LaunchValidationError::InvalidRatio);
    }
    Ok(ratio_millis as u16)
}

fn normalize_environment(
    overrides: Option<&BTreeMap<String, String>>,
    field: &'static str,
    inherited: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, LaunchValidationError> {
    let mut environment = inherited.clone();
    if let Some(overrides) = overrides {
        if overrides.len() > MAX_LAUNCH_ENVIRONMENT_ENTRIES {
            return Err(LaunchValidationError::LimitExceeded {
                field,
                limit: MAX_LAUNCH_ENVIRONMENT_ENTRIES,
            });
        }
        for (name, value) in overrides {
            validate_environment_name(name)?;
            validate_string(value, field, MAX_LAUNCH_STRING_BYTES)?;
            environment.insert(name.clone(), value.clone());
        }
    }
    if environment.len() > MAX_LAUNCH_ENVIRONMENT_ENTRIES {
        return Err(LaunchValidationError::LimitExceeded {
            field: "effective environment",
            limit: MAX_LAUNCH_ENVIRONMENT_ENTRIES,
        });
    }
    Ok(environment)
}

fn validate_optional_name(
    value: Option<&str>,
    field: &'static str,
) -> Result<(), LaunchValidationError> {
    if let Some(value) = value {
        validate_string(value, field, MAX_LAUNCH_STRING_BYTES)?;
        if value.is_empty() {
            return Err(LaunchValidationError::EmptyValue { field });
        }
    }
    Ok(())
}

fn validate_optional_string(
    value: Option<&str>,
    field: &'static str,
    limit: usize,
) -> Result<(), LaunchValidationError> {
    if let Some(value) = value {
        validate_string(value, field, limit)?;
    }
    Ok(())
}

fn validate_cwd(cwd: &str, field: &'static str) -> Result<(), LaunchValidationError> {
    validate_string(cwd, field, MAX_LAUNCH_CWD_BYTES)?;
    if cwd.is_empty() {
        return Err(LaunchValidationError::EmptyValue { field });
    }
    Ok(())
}

fn validate_argv(argv: Option<&[String]>) -> Result<(), LaunchValidationError> {
    let Some(argv) = argv else {
        return Ok(());
    };
    if argv.is_empty() || argv.first().is_none_or(String::is_empty) {
        return Err(LaunchValidationError::InvalidArgumentVector);
    }
    if argv.len() > MAX_LAUNCH_ARGUMENTS {
        return Err(LaunchValidationError::LimitExceeded {
            field: "argv",
            limit: MAX_LAUNCH_ARGUMENTS,
        });
    }
    for argument in argv {
        validate_string(argument, "argv", MAX_LAUNCH_STRING_BYTES)?;
    }
    Ok(())
}

fn validate_environment_name(name: &str) -> Result<(), LaunchValidationError> {
    if name.is_empty() || name.contains('=') {
        return Err(LaunchValidationError::InvalidEnvironmentName {
            name: name.to_owned(),
        });
    }
    validate_string(name, "environment name", MAX_LAUNCH_STRING_BYTES)
}

fn validate_string(
    value: &str,
    field: &'static str,
    limit: usize,
) -> Result<(), LaunchValidationError> {
    if value.contains('\0') {
        return Err(LaunchValidationError::ContainsNul { field });
    }
    if value.len() > limit {
        return Err(LaunchValidationError::LimitExceeded { field, limit });
    }
    Ok(())
}

fn validate_encoded_launch_pane_size(
    command: Option<&str>,
    argv: Option<&[String]>,
    environment: &BTreeMap<String, String>,
) -> Result<(), LaunchValidationError> {
    match encoded_launch_pane_bytes(command, argv, environment) {
        Some(bytes) if bytes <= MAX_LAUNCH_ENCODED_PANE_BYTES => Ok(()),
        Some(_) | None => Err(LaunchValidationError::EncodedCommandTooLarge {
            limit: MAX_LAUNCH_ENCODED_PANE_BYTES,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    fn pane(cwd: Option<&str>) -> PaneLaunchDescriptor {
        PaneLaunchDescriptor::Pane(PaneLaunch {
            cwd: cwd.map(str::to_owned),
            ..PaneLaunch::default()
        })
    }

    fn relative_cwd_layout() -> PaneLaunchDescriptor {
        PaneLaunchDescriptor::Split(SplitLaunch {
            direction: LaunchSplitDirection::Right,
            ratio: None,
            first: Box::new(pane(Some("child"))),
            second: Box::new(PaneLaunchDescriptor::Split(SplitLaunch {
                direction: LaunchSplitDirection::Down,
                ratio: None,
                first: Box::new(pane(Some("child"))),
                second: Box::new(pane(None)),
            })),
        })
    }

    #[test]
    fn recursive_layout_normalizes_in_declaration_order() {
        let descriptor = SessionLaunchDescriptor {
            name: Some("review".to_owned()),
            focus: Some(true),
            default_cwd: Some("/repo".to_owned()),
            environment: Some(BTreeMap::from([("SESSION".to_owned(), "1".to_owned())])),
            windows: Some(vec![
                WindowLaunchDescriptor {
                    name: Some("code".to_owned()),
                    focus: Some(true),
                    layout: PaneLaunchDescriptor::Split(SplitLaunch {
                        direction: LaunchSplitDirection::Right,
                        ratio: Some(0.6),
                        first: Box::new(pane(None)),
                        second: Box::new(PaneLaunchDescriptor::Split(SplitLaunch {
                            direction: LaunchSplitDirection::Down,
                            ratio: None,
                            first: Box::new(pane(Some("/repo/docs"))),
                            second: Box::new(pane(Some("/repo/tests"))),
                        })),
                    }),
                },
                WindowLaunchDescriptor {
                    name: Some("shell".to_owned()),
                    focus: None,
                    layout: pane(Some("/repo")),
                },
            ]),
        };

        let normalized = descriptor
            .normalize_for_remote()
            .expect("normalize recursive layout");
        let plan = normalized.mux_plan("review");

        assert_eq!(normalized.pane_count(), 4);
        assert_eq!(normalized.focused_window_index(), 0);
        assert_eq!(plan.windows.len(), 2);
        assert_eq!(plan.windows[0].layout.pane_count(), 3);
        let MuxPaneLaunchPlan::Split(root) = &plan.windows[0].layout else {
            panic!("expected root split");
        };
        assert_eq!(root.ratio_millis, 600);
        let MuxPaneLaunchPlan::Split(nested) = root.second.as_ref() else {
            panic!("expected nested split");
        };
        let MuxPaneLaunchPlan::Pane(overridden) = nested.first.as_ref() else {
            panic!("expected overridden pane");
        };
        assert_eq!(overridden.cwd, "/repo/docs");
        assert_eq!(overridden.environment["SESSION"], "1");
        assert_eq!(
            normalized.leaf_cwds().collect::<Vec<_>>(),
            ["/repo", "/repo/docs", "/repo/tests", "/repo"]
        );
    }

    #[test]
    fn remote_relative_pane_cwds_inherit_the_session_default_recursively() {
        let descriptor = SessionLaunchDescriptor {
            default_cwd: Some("/repo".to_owned()),
            windows: Some(vec![WindowLaunchDescriptor {
                layout: relative_cwd_layout(),
                ..WindowLaunchDescriptor::default()
            }]),
            ..SessionLaunchDescriptor::default()
        };

        let normalized = descriptor
            .normalize_for_remote()
            .expect("normalize remote relative pane cwd");

        assert_eq!(
            normalized.leaf_cwds().collect::<Vec<_>>(),
            ["/repo/child", "/repo/child", "/repo"]
        );
    }

    #[test]
    fn local_relative_pane_cwds_inherit_before_canonicalization() {
        let temporary_directory = tempfile::tempdir().expect("create temporary directory");
        let repo = temporary_directory.path().join("repo");
        let child = repo.join("child");
        fs::create_dir_all(&child).expect("create inherited child directory");
        let descriptor = SessionLaunchDescriptor {
            default_cwd: Some(repo.to_string_lossy().into_owned()),
            windows: Some(vec![WindowLaunchDescriptor {
                layout: relative_cwd_layout(),
                ..WindowLaunchDescriptor::default()
            }]),
            ..SessionLaunchDescriptor::default()
        };

        let normalized = descriptor
            .normalize()
            .expect("normalize local relative pane cwd");
        let canonical_repo = repo.canonicalize().expect("canonicalize repository");
        let canonical_child = child.canonicalize().expect("canonicalize child directory");
        let canonical_repo = canonical_repo.to_str().expect("repository path is UTF-8");
        let canonical_child = canonical_child.to_str().expect("child path is UTF-8");

        assert_eq!(
            normalized.leaf_cwds().collect::<Vec<_>>(),
            [canonical_child, canonical_child, canonical_repo]
        );
        assert!(
            normalized
                .panes_depth_first()
                .all(|pane| pane.local_directory().is_some())
        );
    }

    #[test]
    fn command_is_preserved_as_the_normative_process_intent() {
        let descriptor = SessionLaunchDescriptor {
            default_cwd: Some("/srv/app".to_owned()),
            windows: Some(vec![WindowLaunchDescriptor {
                layout: PaneLaunchDescriptor::Pane(PaneLaunch {
                    command: Some("exec ./serve --port 8080".to_owned()),
                    ..PaneLaunch::default()
                }),
                ..WindowLaunchDescriptor::default()
            }]),
            ..SessionLaunchDescriptor::default()
        };

        let plan = descriptor
            .normalize_for_remote()
            .expect("normalize remote command")
            .mux_plan("service");
        let MuxPaneLaunchPlan::Pane(pane) = &plan.windows[0].layout else {
            panic!("expected leaf pane");
        };

        assert_eq!(pane.command.as_deref(), Some("exec ./serve --port 8080"));
        assert_eq!(pane.argv, None);
    }

    #[test]
    fn command_and_argv_are_mutually_exclusive() {
        let descriptor = SessionLaunchDescriptor {
            default_cwd: Some("/srv/app".to_owned()),
            windows: Some(vec![WindowLaunchDescriptor {
                layout: PaneLaunchDescriptor::Pane(PaneLaunch {
                    command: Some("exec ./serve".to_owned()),
                    argv: Some(vec!["./serve".to_owned()]),
                    ..PaneLaunch::default()
                }),
                ..WindowLaunchDescriptor::default()
            }]),
            ..SessionLaunchDescriptor::default()
        };

        assert_eq!(
            descriptor.normalize_for_remote(),
            Err(LaunchValidationError::CommandAndArgv)
        );
    }

    #[test]
    fn descriptor_rejects_unknown_fields_at_every_launch_level() {
        assert!(
            serde_json::from_str::<SessionLaunchDescriptor>(r#"{"unknown":true}"#).is_err(),
            "session descriptor must reject unknown fields"
        );
        assert!(
            serde_json::from_str::<SessionLaunchDescriptor>(
                r#"{"windows":[{"layout":{"kind":"pane","command":"echo ok","unknown":true}}]}"#
            )
            .is_err(),
            "pane descriptor must reject unknown fields"
        );
        assert!(
            serde_json::from_str::<SessionLaunchDescriptor>(
                r#"{"windows":[{"layout":{"kind":"split","direction":"right","first":{"kind":"pane"},"second":{"kind":"pane"},"unknown":true}}]}"#
            )
            .is_err(),
            "split descriptor must reject unknown fields"
        );
    }

    #[test]
    fn remote_launch_never_uses_the_local_home_as_a_default() {
        assert_eq!(
            SessionLaunchDescriptor::default().normalize_for_remote(),
            Err(LaunchValidationError::RemoteDefaultCwdRequired)
        );
    }

    #[test]
    fn local_cwd_retains_its_typed_directory_identity() {
        let normalized = SessionLaunchDescriptor {
            default_cwd: Some(".".to_owned()),
            ..SessionLaunchDescriptor::default()
        }
        .normalize_with_default_cwd("/unused")
        .expect("normalize local directory");
        let directory = normalized
            .default_local_directory()
            .expect("local launch retains DirectoryRef");

        assert_eq!(
            directory.canonical_path.to_str(),
            Some(normalized.default_cwd())
        );
        assert!(
            normalized
                .panes_depth_first()
                .all(|pane| pane.local_directory().is_some())
        );
    }

    #[test]
    fn ambiguous_window_focus_fails_before_planning() {
        let descriptor = SessionLaunchDescriptor {
            default_cwd: Some("/repo".to_owned()),
            windows: Some(vec![
                WindowLaunchDescriptor {
                    focus: Some(true),
                    ..WindowLaunchDescriptor::default()
                },
                WindowLaunchDescriptor {
                    focus: Some(true),
                    ..WindowLaunchDescriptor::default()
                },
            ]),
            ..SessionLaunchDescriptor::default()
        };

        assert_eq!(
            descriptor.normalize_with_default_cwd("/unused"),
            Err(LaunchValidationError::AmbiguousFocus { scope: "windows" })
        );
    }

    #[test]
    fn explicit_default_cwd_keeps_normalization_deterministic() {
        let descriptor = SessionLaunchDescriptor {
            windows: Some(vec![WindowLaunchDescriptor {
                layout: pane(None),
                ..WindowLaunchDescriptor::default()
            }]),
            ..SessionLaunchDescriptor::default()
        };

        let first = descriptor
            .normalize_with_default_cwd("/one")
            .expect("first normalization");
        let second = descriptor
            .normalize_with_default_cwd("/one")
            .expect("second normalization");

        assert_eq!(first, second);
        assert_eq!(first.mux_plan("same"), second.mux_plan("same"));
    }

    #[test]
    fn pane_process_payload_limit_accounts_for_tmux_shell_quote_expansion() {
        // A single quote grows once in tmux's argv shell adapter and again in the remote shell.
        // The encoded size for this one-element argv is 16 + 14 * quote_count.
        let quote_count = (MAX_LAUNCH_ENCODED_PANE_BYTES - 16) / 14;
        let descriptor = |quote_count| SessionLaunchDescriptor {
            default_cwd: Some("/repo".to_owned()),
            windows: Some(vec![WindowLaunchDescriptor {
                layout: PaneLaunchDescriptor::Pane(PaneLaunch {
                    argv: Some(vec!["'".repeat(quote_count)]),
                    ..PaneLaunch::default()
                }),
                ..WindowLaunchDescriptor::default()
            }]),
            ..SessionLaunchDescriptor::default()
        };

        assert!(descriptor(quote_count).normalize_for_remote().is_ok());
        assert_eq!(
            descriptor(quote_count + 1).normalize_for_remote(),
            Err(LaunchValidationError::EncodedCommandTooLarge {
                limit: MAX_LAUNCH_ENCODED_PANE_BYTES,
            })
        );
    }
}
