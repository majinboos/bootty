use super::trigger_edit::{combo_has_modifier_sides, combo_is_prefixed, parse_trigger_flags};
use crate::ui::settings::surface::writeback::SettingsWriteback;
use bootty_config::config::{BoottyConfig, InputConfig, split_keybind_entry};

/// Which keybind list is being edited: the global list, one of the per-backend lists, or the
/// sidebar navigation list (which has its own action vocabulary).
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
pub(in crate::ui::settings) enum KeybindScope {
    #[default]
    Global,
    Herdr,
    Native,
    Rmux,
    #[cfg(not(windows))]
    Tmux,
    Sidebar,
}

impl KeybindScope {
    pub(super) const ALL: &'static [(KeybindScope, &'static str)] = &[
        (Self::Global, "Global"),
        (Self::Herdr, "Herdr"),
        (Self::Native, "Native"),
        (Self::Rmux, "Rmux"),
        #[cfg(not(windows))]
        (Self::Tmux, "Tmux"),
        (Self::Sidebar, "Sidebar"),
    ];

    fn path(self) -> &'static [&'static str] {
        match self {
            Self::Global => &["input", "keybind"],
            Self::Herdr => &["input", "backend-keybind", "herdr"],
            Self::Native => &["input", "backend-keybind", "native"],
            Self::Rmux => &["input", "backend-keybind", "rmux"],
            #[cfg(not(windows))]
            Self::Tmux => &["input", "backend-keybind", "tmux"],
            Self::Sidebar => &["input", "sidebar-keybind"],
        }
    }

    pub(super) fn effective_prefix(self, input: &InputConfig) -> Option<String> {
        matches!(self, Self::Global | Self::Native | Self::Rmux)
            .then(|| input.effective_prefix())
            .flatten()
    }

    /// Whether `entry` (`trigger=action`) is a valid binding for this list. The sidebar list uses
    /// its own trigger/action grammar rather than the app-level binding parser.
    pub(super) fn entry_is_valid(self, trigger: &str, action: &str) -> bool {
        if self == Self::Sidebar {
            trigger
                .parse::<bootty_winit::input_binding::BindingTrigger>()
                .is_ok()
                && SIDEBAR_ACTION_INFO
                    .iter()
                    .any(|(name, _, _)| *name == action)
        } else {
            bootty_winit::input_binding::parse_binding_elements(&format!("{trigger}={action}"))
                .is_ok()
        }
    }
}

/// Action picker options for `scope`: app/backend lists draw their vocabulary
/// (titles + descriptions) from the shared [`crate::action_catalog`] — one source
/// of truth with the command palette; the sidebar list has its own small set.
pub(super) fn action_options(
    scope: KeybindScope,
) -> Vec<(&'static str, &'static str, &'static str)> {
    match scope {
        KeybindScope::Sidebar => SIDEBAR_ACTION_INFO.to_vec(),
        _ => crate::action_catalog::Command::all()
            .map(|command| (command.action(), command.title(), command.description()))
            .collect(),
    }
}

/// One editable binding: a trigger (one combo, or a `>`-joined chord), an action, and editor-only
/// state for whether newly recorded modifiers should keep left/right side information and whether
/// recording composes the trigger as `{prefix}>{key}` instead of capturing literally.
#[derive(Default)]
pub(in crate::ui::settings) struct BindingRow {
    pub trigger: String,
    pub action: String,
    pub side_sensitive: bool,
    pub prefixed: bool,
}

impl BindingRow {
    /// `None` is an incomplete draft; complete rows are either valid or invalid.
    pub(super) fn validity(&self, scope: KeybindScope) -> Option<bool> {
        let trigger = self.trigger.trim();
        let action = self.action.trim();
        (!trigger.is_empty() && !action.is_empty()).then(|| scope.entry_is_valid(trigger, action))
    }

    fn entry(&self, scope: KeybindScope) -> Option<String> {
        self.validity(scope)?
            .then(|| format!("{}={}", self.trigger.trim(), self.action.trim()))
    }
}

/// In-progress chord capture: steps accumulate until `deadline` passes with no new key.
pub(in crate::ui::settings) struct ChordCapture {
    pub row: usize,
    pub steps: Vec<String>,
    pub deadline: Option<f64>,
}

/// Actions accepted in the sidebar navigation list (see `sidebar_action` in `app_actions`), with
/// titles + descriptions for the picker. This list has its own vocabulary, distinct from the
/// app-action catalog.
const SIDEBAR_ACTION_INFO: &[(&str, &str, &str)] = &[
    ("ignore", "Ignore", "Do nothing — let the keys pass through"),
    (
        "previous_session",
        "Previous Session",
        "Move the sidebar highlight up",
    ),
    (
        "next_session",
        "Next Session",
        "Move the sidebar highlight down",
    ),
    (
        "activate_session",
        "Activate Session",
        "Open the highlighted session",
    ),
    (
        "focus_terminal",
        "Focus Terminal",
        "Return focus to the terminal",
    ),
];

/// Load the editable rows for `scope` from the draft document, not from the accepted config, so a
/// user override list never shows the built-in defaults as editable rows.
pub(super) fn read_scope_entries(
    writeback: &SettingsWriteback,
    input: &InputConfig,
    scope: KeybindScope,
) -> (bool, Vec<BindingRow>) {
    let prefix = scope.effective_prefix(input);
    let Some(entries) = writeback.string_array(scope.path()) else {
        return (false, Vec::new());
    };

    let mut clear = false;
    let mut rows = Vec::new();
    for entry in entries {
        if entry == "clear" {
            clear = true;
            continue;
        }
        let (trigger, action) = split_keybind_entry(&entry).map_or_else(
            || (entry.to_owned(), String::new()),
            |(trigger, action)| (trigger.to_owned(), action.to_owned()),
        );
        let (_, combo) = parse_trigger_flags(&trigger);
        rows.push(BindingRow {
            side_sensitive: combo_has_modifier_sides(&combo),
            prefixed: prefix
                .as_deref()
                .is_some_and(|prefix| combo_is_prefixed(&combo, prefix)),
            trigger,
            action,
        });
    }
    (clear, rows)
}

pub(super) fn write_scope(
    writeback: &mut SettingsWriteback,
    scope: KeybindScope,
    clear: bool,
    rows: &[BindingRow],
) {
    let mut entries: Vec<String> = Vec::new();
    if clear {
        entries.push("clear".to_owned());
    }
    // Skip incomplete and invalid drafts so they never make the config fail to reload.
    entries.extend(rows.iter().filter_map(|row| row.entry(scope)));
    writeback.set_strings(scope.path(), &entries);
}

pub(super) fn effective_bindings(config: &BoottyConfig, scope: KeybindScope) -> Vec<String> {
    use bootty_config::config::MultiplexerBackendConfig;
    let input = &config.input;
    match scope {
        KeybindScope::Global => input.keybinds_for_backend(config.multiplexer.backend),
        KeybindScope::Herdr => input.keybinds_for_backend(MultiplexerBackendConfig::Herdr),
        KeybindScope::Native => input.keybinds_for_backend(MultiplexerBackendConfig::Native),
        KeybindScope::Rmux => input.keybinds_for_backend(MultiplexerBackendConfig::Rmux),
        #[cfg(not(windows))]
        KeybindScope::Tmux => input.keybinds_for_backend(MultiplexerBackendConfig::Tmux),
        KeybindScope::Sidebar => input.sidebar_keybind.clone(),
    }
}
