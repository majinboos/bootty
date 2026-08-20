//! Single source of truth for the user-facing command vocabulary: the actions
//! the command palette runs and the keybind editor offers, each with a human
//! title and description (via `strum` message attributes) plus an icon.
//!
//! Titles/descriptions live on the enum so they are defined once. The dispatch
//! name (`Command::action`) maps to the binding-action string the rest of the
//! app already understands (`app_actions::keybind_action_for_name`), so adding a
//! command here surfaces it in both the palette and the editor without touching
//! the dispatch layer.

use strum::{EnumIter, EnumMessage, IntoEnumIterator};

/// A user-facing command. Declaration order is the display order in both the
/// palette and the editor dropdown: palette commands first, editor-only ones
/// (parameterized or low-level) after.
#[derive(Clone, Copy, Debug, PartialEq, Eq, EnumIter, EnumMessage)]
pub enum Command {
    #[strum(
        message = "New Session",
        detailed_message = "Pick a directory or worktree and start a session"
    )]
    NewSession,
    #[strum(
        message = "Switch Session",
        detailed_message = "Fuzzy-find and jump to an open session"
    )]
    SwitchSession,
    #[strum(
        message = "Rename Session",
        detailed_message = "Rename the current session"
    )]
    RenameSession,
    #[strum(
        message = "Ditch Session",
        detailed_message = "Close the session and optionally remove its worktree"
    )]
    DitchSession,
    #[strum(
        message = "New Space",
        detailed_message = "Create and activate a new space"
    )]
    CreateSpace,
    #[strum(message = "Edit Space", detailed_message = "Edit the active space")]
    EditSpace,
    #[strum(message = "Close Space", detailed_message = "Close the active space")]
    CloseSpace,
    #[strum(message = "Next Space", detailed_message = "Activate the next space")]
    NextSpace,
    #[strum(
        message = "Previous Space",
        detailed_message = "Activate the previous space"
    )]
    PreviousSpace,
    #[strum(
        message = "Next Session",
        detailed_message = "Activate the next session"
    )]
    NextSession,
    #[strum(
        message = "Previous Session",
        detailed_message = "Activate the previous session"
    )]
    PreviousSession,
    #[strum(
        message = "Last Session",
        detailed_message = "Toggle back to the most recent session"
    )]
    LastSession,
    #[strum(
        message = "New Tab",
        detailed_message = "Open a new tab in the current session"
    )]
    NewTab,
    #[strum(message = "Next Tab", detailed_message = "Activate the next tab")]
    NextTab,
    #[strum(
        message = "Previous Tab",
        detailed_message = "Activate the previous tab"
    )]
    PreviousTab,
    #[strum(
        message = "Last Tab",
        detailed_message = "Toggle back to the most recently used tab"
    )]
    LastTab,
    #[strum(
        message = "Move Tab Left",
        detailed_message = "Reorder the current tab one position left"
    )]
    MoveTabLeft,
    #[strum(
        message = "Move Tab Right",
        detailed_message = "Reorder the current tab one position right"
    )]
    MoveTabRight,
    #[strum(message = "Rename Tab", detailed_message = "Rename the current tab")]
    RenameTab,
    #[strum(
        message = "Split Right",
        detailed_message = "Split the current pane horizontally"
    )]
    SplitRight,
    #[strum(
        message = "Split Down",
        detailed_message = "Split the current pane vertically"
    )]
    SplitDown,
    #[strum(
        message = "Next Pane",
        detailed_message = "Move focus to the next pane"
    )]
    NextPane,
    #[strum(
        message = "Previous Pane",
        detailed_message = "Move focus to the previous pane"
    )]
    PreviousPane,
    #[strum(
        message = "Toggle Pane Zoom",
        detailed_message = "Zoom the focused pane to fill the window, or restore it"
    )]
    TogglePaneZoom,
    #[strum(message = "Kill Pane", detailed_message = "Close the focused pane")]
    KillPane,
    #[strum(
        message = "Close Pane",
        detailed_message = "Close the active pane, cascading to the tab"
    )]
    ClosePane,
    #[strum(
        message = "New Window",
        detailed_message = "Open a new top-level window"
    )]
    NewWindow,
    #[strum(
        message = "Toggle Sidebar",
        detailed_message = "Show or hide the session sidebar"
    )]
    ToggleSidebar,
    #[strum(
        message = "Focus Sidebar",
        detailed_message = "Move keyboard focus to the sidebar"
    )]
    FocusSidebar,
    #[strum(
        message = "Toggle Fullscreen",
        detailed_message = "Enter or leave fullscreen"
    )]
    ToggleFullscreen,
    #[strum(
        message = "Scroll to Top",
        detailed_message = "Jump to the top of the scrollback"
    )]
    ScrollToTop,
    #[strum(
        message = "Scroll to Bottom",
        detailed_message = "Jump to the latest output"
    )]
    ScrollToBottom,
    #[strum(
        message = "Copy Mode",
        detailed_message = "Enter tmux-style scrollback navigation and text selection"
    )]
    CopyMode,
    #[strum(
        message = "Increase Font Size",
        detailed_message = "Make the terminal text larger"
    )]
    IncreaseFontSize,
    #[strum(
        message = "Decrease Font Size",
        detailed_message = "Make the terminal text smaller"
    )]
    DecreaseFontSize,
    #[strum(
        message = "Reset Font Size",
        detailed_message = "Restore the configured font size"
    )]
    ResetFontSize,
    #[strum(
        message = "Find in Terminal",
        detailed_message = "Search the terminal scrollback"
    )]
    Find,
    #[strum(
        message = "Keyboard Shortcuts",
        detailed_message = "Browse the active keybindings"
    )]
    KeyboardShortcuts,
    #[strum(
        message = "Use System Appearance",
        detailed_message = "Follow the operating system light/dark appearance"
    )]
    UseSystemAppearance,
    #[strum(
        message = "Use Light Appearance",
        detailed_message = "Switch Bootty to the configured light appearance branch"
    )]
    UseLightAppearance,
    #[strum(
        message = "Use Dark Appearance",
        detailed_message = "Switch Bootty to the configured dark appearance branch"
    )]
    UseDarkAppearance,
    #[strum(
        message = "Switch Theme",
        detailed_message = "Pick a theme for the active light or dark appearance branch"
    )]
    SwitchTheme,
    #[strum(message = "Settings", detailed_message = "Open the settings surface")]
    OpenSettings,
    #[strum(
        message = "Reload Config",
        detailed_message = "Re-read the config file from disk"
    )]
    ReloadConfig,
    #[strum(message = "Quit Bootty", detailed_message = "Close the application")]
    Quit,

    // Editor-only below: parameterized or low-level actions the palette omits.
    #[strum(
        message = "Command Palette",
        detailed_message = "Search and run any command"
    )]
    CommandPalette,
    #[strum(
        message = "Close Window",
        detailed_message = "Close the current window"
    )]
    CloseWindow,
    #[strum(
        message = "Ignore",
        detailed_message = "Do nothing — mask a default binding so the keys pass through"
    )]
    Ignore,
    #[strum(message = "Select Tab", detailed_message = "Jump to tab N (value 1–9)")]
    SelectTab,
    #[strum(
        message = "Move Tab",
        detailed_message = "Reorder the current tab by N"
    )]
    MoveTab,
    #[strum(
        message = "Select Pane",
        detailed_message = "Focus the pane in a direction (left/right/up/down)"
    )]
    SelectPane,
    #[strum(
        message = "Select Session",
        detailed_message = "Jump to session N (value 1–9)"
    )]
    SelectSession,
    #[strum(
        message = "Select Space",
        detailed_message = "Jump to space N (value 1–9)"
    )]
    SelectSpace,
    #[strum(
        message = "Move Session",
        detailed_message = "Reorder the current session by N"
    )]
    MoveSession,
    #[strum(message = "Scroll Page Up", detailed_message = "Scroll up one page")]
    ScrollPageUp,
    #[strum(
        message = "Scroll Page Down",
        detailed_message = "Scroll down one page"
    )]
    ScrollPageDown,
    #[strum(
        message = "Scroll Lines",
        detailed_message = "Scroll by N lines (negative scrolls up)"
    )]
    ScrollPageLines,
    #[strum(
        message = "Set Font Size",
        detailed_message = "Set the font size to N points"
    )]
    SetFontSize,
    #[strum(
        message = "Search Terminal",
        detailed_message = "Search the terminal scrollback for text"
    )]
    Search,
    #[strum(
        message = "Search Selection",
        detailed_message = "Search the terminal scrollback for the selected text"
    )]
    SearchSelection,
    #[strum(
        message = "Next Search Match",
        detailed_message = "Move to the next terminal search match"
    )]
    NavigateSearchNext,
    #[strum(
        message = "Previous Search Match",
        detailed_message = "Move to the previous terminal search match"
    )]
    NavigateSearchPrevious,
    #[strum(
        message = "Close Terminal Search",
        detailed_message = "Close the terminal search surface"
    )]
    EndSearch,
    #[strum(
        message = "Copy",
        detailed_message = "Copy the selection to the clipboard"
    )]
    Copy,
    #[strum(message = "Paste", detailed_message = "Paste from the clipboard")]
    Paste,
    #[strum(
        message = "Send CSI",
        detailed_message = "Write a CSI escape sequence to the terminal"
    )]
    SendCsi,
    #[strum(
        message = "Send ESC",
        detailed_message = "Write an ESC sequence to the terminal"
    )]
    SendEsc,
    #[strum(
        message = "Send Text",
        detailed_message = "Write literal text to the terminal"
    )]
    SendText,
}

impl Command {
    /// Iterate every command in display order.
    pub fn all() -> impl Iterator<Item = Self> {
        Self::iter()
    }

    /// Human title (e.g. "Rename Session").
    pub fn title(self) -> &'static str {
        self.get_message().unwrap_or("")
    }

    /// One-line description for the palette/editor.
    pub fn description(self) -> &'static str {
        self.get_detailed_message().unwrap_or("")
    }

    /// The binding action string used by the keybind editor and, for concrete
    /// palette commands, by dispatch.
    pub fn action(self) -> &'static str {
        match self {
            Self::NewSession => "new_mux_session",
            Self::SwitchSession => "session_picker",
            Self::RenameSession => "rename_session",
            Self::DitchSession => "ditch_session",
            Self::CreateSpace => "create_space",
            Self::EditSpace => "edit_space",
            Self::CloseSpace => "close_space",
            Self::NextSpace => "next_space",
            Self::PreviousSpace => "previous_space",
            Self::NextSession => "next_session",
            Self::PreviousSession => "previous_session",
            Self::LastSession => "last_session",
            Self::NewTab => "new_tab",
            Self::NextTab => "next_tab",
            Self::PreviousTab => "previous_tab",
            Self::LastTab => "last_tab",
            Self::MoveTabLeft => "move_tab:-1",
            Self::MoveTabRight => "move_tab:1",
            Self::RenameTab => "rename_tab",
            Self::SplitRight => "split_right",
            Self::SplitDown => "split_down",
            Self::NextPane => "next_pane",
            Self::PreviousPane => "previous_pane",
            Self::TogglePaneZoom => "toggle_pane_zoom",
            Self::KillPane => "kill_pane",
            Self::ClosePane => "close_surface",
            Self::NewWindow => "new_window",
            Self::ToggleSidebar => "toggle_sidebar_visibility",
            Self::FocusSidebar => "toggle_sidebar_focus",
            Self::ToggleFullscreen => "toggle_fullscreen",
            Self::ScrollToTop => "scroll_to_top",
            Self::ScrollToBottom => "scroll_to_bottom",
            Self::CopyMode => "copy_mode",
            Self::IncreaseFontSize => "increase_font_size",
            Self::DecreaseFontSize => "decrease_font_size",
            Self::ResetFontSize => "reset_font_size",
            Self::Find => "start_search",
            Self::KeyboardShortcuts => "show_keybinds",
            Self::UseSystemAppearance => "change_appearance:system",
            Self::UseLightAppearance => "change_appearance:light",
            Self::UseDarkAppearance => "change_appearance:dark",
            Self::SwitchTheme => "switch_theme",
            Self::OpenSettings => "open_settings",
            Self::ReloadConfig => "reload_config",
            Self::Quit => "quit",
            Self::CommandPalette => "command_palette",
            Self::CloseWindow => "close_window",
            Self::Ignore => "ignore",
            Self::SelectTab => "select_tab",
            Self::MoveTab => "move_tab",
            Self::SelectPane => "select_pane",
            Self::SelectSession => "select_session",
            Self::SelectSpace => "select_space",
            Self::MoveSession => "move_session",
            Self::ScrollPageUp => "scroll_page_up",
            Self::ScrollPageDown => "scroll_page_down",
            Self::ScrollPageLines => "scroll_page_lines",
            Self::SetFontSize => "set_font_size",
            Self::Search => "search",
            Self::SearchSelection => "search_selection",
            Self::NavigateSearchNext => "navigate_search:next",
            Self::NavigateSearchPrevious => "navigate_search:previous",
            Self::EndSearch => "end_search",
            Self::Copy => "copy_to_clipboard",
            Self::Paste => "paste_from_clipboard",
            Self::SendCsi => "csi",
            Self::SendEsc => "esc",
            Self::SendText => "text",
        }
    }

    /// Lucide icon slug shown in the palette.
    pub fn icon(self) -> &'static str {
        match self {
            Self::NewSession => "square-plus",
            Self::SwitchSession => "terminal",
            Self::RenameSession => "pencil",
            Self::DitchSession => "trash-2",
            Self::CreateSpace => "plus",
            Self::EditSpace => "pencil",
            Self::CloseSpace => "x",
            Self::NextSession => "chevron-down",
            Self::NextSpace => "chevron-right",
            Self::PreviousSpace => "chevron-left",
            Self::PreviousSession => "chevron-up",
            Self::LastSession => "history",
            Self::NewTab => "plus",
            Self::NextTab => "chevron-right",
            Self::PreviousTab => "chevron-left",
            Self::LastTab => "arrow-right-to-line",
            Self::RenameTab => "pencil",
            Self::SplitRight => "columns-2",
            Self::SplitDown => "rows-2",
            Self::NextPane => "layout-grid",
            Self::PreviousPane => "layout-grid",
            Self::TogglePaneZoom => "maximize-2",
            Self::KillPane => "x",
            Self::ClosePane => "square-x",
            Self::NewWindow => "app-window",
            Self::ToggleSidebar => "panel-left",
            Self::FocusSidebar => "panel-left-open",
            Self::ToggleFullscreen => "maximize",
            Self::ScrollToTop => "arrow-up-to-line",
            Self::ScrollToBottom => "arrow-down-to-line",
            Self::CopyMode => "copy",
            Self::IncreaseFontSize => "zoom-in",
            Self::DecreaseFontSize => "zoom-out",
            Self::ResetFontSize | Self::SetFontSize => "type",
            Self::Find
            | Self::Search
            | Self::SearchSelection
            | Self::NavigateSearchNext
            | Self::NavigateSearchPrevious
            | Self::EndSearch => "search",
            Self::KeyboardShortcuts => "keyboard",
            Self::UseSystemAppearance | Self::UseLightAppearance | Self::UseDarkAppearance => {
                "sun-moon"
            }
            Self::SwitchTheme => "palette",
            Self::OpenSettings => "settings",
            Self::ReloadConfig => "refresh-cw",
            Self::Quit => "power",
            Self::CommandPalette => "search",
            Self::CloseWindow => "x",
            Self::Ignore => "ban",
            Self::SelectTab | Self::SelectSession | Self::SelectSpace => "hash",
            Self::MoveTab | Self::MoveTabLeft | Self::MoveTabRight => "move-horizontal",
            Self::SelectPane => "layout",
            Self::MoveSession => "move-vertical",
            Self::ScrollPageUp => "chevrons-up",
            Self::ScrollPageDown => "chevrons-down",
            Self::ScrollPageLines => "list",
            Self::Copy => "copy",
            Self::Paste => "clipboard",
            Self::SendCsi | Self::SendEsc | Self::SendText => "terminal",
        }
    }

    /// The exact action string the command palette dispatches, or `None` for
    /// editor-only commands (parameterized or context-dependent). Most commands
    /// dispatch their bare [`Self::action`]; the font-size steps bake in a value.
    pub fn palette_action(self) -> Option<&'static str> {
        match self {
            Self::IncreaseFontSize => Some("increase_font_size:1"),
            Self::DecreaseFontSize => Some("decrease_font_size:1"),
            Self::MoveTabLeft => Some("move_tab:-1"),
            Self::MoveTabRight => Some("move_tab:1"),
            Self::CommandPalette
            | Self::CloseWindow
            | Self::Ignore
            | Self::SelectTab
            | Self::MoveTab
            | Self::SelectPane
            | Self::SelectSession
            | Self::SelectSpace
            | Self::MoveSession
            | Self::ScrollPageUp
            | Self::ScrollPageDown
            | Self::ScrollPageLines
            | Self::SetFontSize
            | Self::Search
            | Self::SearchSelection
            | Self::NavigateSearchNext
            | Self::NavigateSearchPrevious
            | Self::EndSearch
            | Self::Copy
            | Self::Paste
            | Self::SendCsi
            | Self::SendEsc
            | Self::SendText => None,
            other => Some(other.action()),
        }
    }

    /// The command whose action string is `name`, for resolving a stored binding row
    /// back to its title/description.
    pub fn from_action(name: &str) -> Option<Self> {
        Self::all().find(|command| command.action() == name)
    }
}
