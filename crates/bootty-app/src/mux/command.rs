#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MuxDirection {
    Left,
    Down,
    Up,
    Right,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MuxCommand {
    ActivateSession {
        session_id: String,
    },
    ActivateNextSession,
    ActivatePreviousSession,
    ActivateLastSession,
    ActivateSessionIndex {
        index: u32,
    },
    MoveSession {
        delta: i32,
    },
    ActivateWindow {
        session_id: String,
        window_id: String,
    },
    NewWindow {
        session_id: String,
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
        delta: i32,
    },
    SplitPane {
        session_id: String,
    },
    SelectPane {
        session_id: String,
        direction: MuxDirection,
    },
    SelectNextPane {
        session_id: String,
    },
    KillPane {
        session_id: String,
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
