use serde::{Deserialize, Serialize};

use crate::snapshot::MuxSessionTag;

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
    CreateProjectSession {
        session_id: String,
        cwd: String,
        /// What to stamp onto the new session. Bootty mints the identity rather than the backend
        /// so a create whose result never came back can be settled by looking for this id.
        tag: MuxSessionTag,
    },
    CreateWorktreeSession {
        session_id: String,
        cwd: String,
        tag: MuxSessionTag,
    },
    RenameSession {
        session_id: String,
        name: String,
    },
    DitchSession {
        session_id: String,
    },
    /// Write `tag` onto a session that already exists: adopting one bootty did not create, or
    /// restoring one whose server restarted and dropped its tag.
    StampSession {
        session_id: String,
        tag: MuxSessionTag,
    },
}

impl MuxCommand {
    /// Whether running the command again is safe when the first attempt's
    /// outcome is unknown. Relative moves double, creates duplicate, toggles
    /// flip back, and closes take the next pane along, so only commands that
    /// name an absolute end state qualify.
    pub fn is_repeatable(&self) -> bool {
        matches!(
            self,
            Self::ActivateWindow { .. }
                | Self::ActivateWindowIndex { .. }
                | Self::RenameWindow { .. }
                | Self::RenameSession { .. }
                | Self::StampSession { .. }
                // Both create-or-reuse a session under a name Bootty minted, so
                // a second attempt adopts what the first one made.
                | Self::CreateProjectSession { .. }
                | Self::CreateWorktreeSession { .. }
        )
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
            Self::KillPane { .. } | Self::ClosePane { .. } => BindingOperation::ClosePane,
            Self::TogglePaneZoom { .. } => BindingOperation::TogglePaneZoom,
            Self::CreateProjectSession { .. } => BindingOperation::CreateProjectSession,
            Self::CreateWorktreeSession { .. } => BindingOperation::CreateWorktreeSession,
            Self::RenameSession { .. } => BindingOperation::RenameSession,
            Self::DitchSession { .. } => BindingOperation::DitchSession,
            Self::StampSession { .. } => BindingOperation::StampSession,
        }
    }
}
