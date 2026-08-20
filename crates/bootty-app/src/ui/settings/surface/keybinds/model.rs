use super::{
    SettingsWindow,
    trigger_edit::{combo_has_modifier_sides, combo_is_prefixed, parse_trigger_flags},
};
use crate::config::split_keybind_entry;

/// Which keybind list is being edited: the global list, one of the per-backend lists, or the
/// sidebar navigation list (which has its own action vocabulary).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(in crate::ui::settings) enum KeybindScope {
    Global,
    Native,
    Rmux,
    #[cfg(not(windows))]
    Tmux,
    Zellij,
    Sidebar,
}

impl KeybindScope {
    #[cfg(not(windows))]
    pub(super) const ALL: &'static [(KeybindScope, &'static str)] = &[
        (Self::Global, "Global"),
        (Self::Native, "Native"),
        (Self::Rmux, "Rmux"),
        (Self::Tmux, "Tmux"),
        (Self::Zellij, "Zellij"),
        (Self::Sidebar, "Sidebar"),
    ];

    #[cfg(windows)]
    pub(super) const ALL: &'static [(KeybindScope, &'static str)] = &[
        (Self::Global, "Global"),
        (Self::Native, "Native"),
        (Self::Rmux, "Rmux"),
        (Self::Zellij, "Zellij"),
        (Self::Sidebar, "Sidebar"),
    ];

    fn path(self) -> &'static [&'static str] {
        match self {
            Self::Global => &["input", "keybind"],
            Self::Native => &["input", "backend-keybind", "native"],
            Self::Rmux => &["input", "backend-keybind", "rmux"],
            #[cfg(not(windows))]
            Self::Tmux => &["input", "backend-keybind", "tmux"],
            Self::Zellij => &["input", "backend-keybind", "zellij"],
            Self::Sidebar => &["input", "sidebar-keybind"],
        }
    }

    /// Whether `entry` (`trigger=action`) is a valid binding for this list. The sidebar list uses
    /// its own trigger/action grammar rather than the app-level binding parser.
    pub(super) fn entry_is_valid(self, trigger: &str, action: &str) -> bool {
        if self == Self::Sidebar {
            trigger
                .parse::<crate::input_binding::BindingTrigger>()
                .is_ok()
                && SIDEBAR_ACTION_INFO
                    .iter()
                    .any(|(name, _, _)| *name == action)
        } else {
            crate::input_binding::parse_binding_elements(&format!("{trigger}={action}")).is_ok()
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

pub(super) fn read_scope_entries(
    win: &SettingsWindow,
    scope: KeybindScope,
) -> (bool, Vec<BindingRow>) {
    let prefix = match scope {
        KeybindScope::Global | KeybindScope::Native | KeybindScope::Rmux => {
            win.config.input.effective_prefix()
        }
        _ => None,
    };
    let Some(entries) = win.writeback.string_array(scope.path()) else {
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
    win: &mut SettingsWindow,
    scope: KeybindScope,
    clear: bool,
    rows: &[BindingRow],
) {
    let mut entries: Vec<String> = Vec::new();
    if clear {
        entries.push("clear".to_owned());
    }
    for row in rows {
        let trigger = row.trigger.trim();
        let action = row.action.trim();
        if trigger.is_empty() || action.is_empty() {
            continue;
        }
        // Skip invalid rows so a half-typed binding never makes the whole config fail to reload.
        if scope.entry_is_valid(trigger, action) {
            entries.push(format!("{trigger}={action}"));
        }
    }
    win.writeback.set_strings(scope.path(), &entries);
}

/// The shortcuts that actually apply for `scope`: backend scopes show the fully merged view
/// (global rows + the backend's own rows, including prefixed chords), matching what the runtime
/// resolves for that backend. Global shows the merge for the currently configured backend, since
/// that's what actually fires while using the app.
pub(super) fn effective_bindings(win: &SettingsWindow, scope: KeybindScope) -> Vec<String> {
    use crate::config::MultiplexerBackendConfig;
    let input = &win.config.input;
    match scope {
        KeybindScope::Global => input.keybinds_for_backend(win.config.multiplexer.backend),
        KeybindScope::Native => input.keybinds_for_backend(MultiplexerBackendConfig::Native),
        KeybindScope::Rmux => input.keybinds_for_backend(MultiplexerBackendConfig::Rmux),
        #[cfg(not(windows))]
        KeybindScope::Tmux => input.keybinds_for_backend(MultiplexerBackendConfig::Tmux),
        KeybindScope::Zellij => input.keybinds_for_backend(MultiplexerBackendConfig::Zellij),
        KeybindScope::Sidebar => input.sidebar_keybind.clone(),
    }
}
