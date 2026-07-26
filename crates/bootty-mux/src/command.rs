use crate::capability::BindingOperation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MuxDirection {
    Left,
    Down,
    Up,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MuxSplitDirection {
    Right,
    Down,
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
        direction: MuxDirection,
    },
    SelectNextPane {
        session_id: String,
    },
    SelectPreviousPane {
        session_id: String,
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
        }
    }
}
