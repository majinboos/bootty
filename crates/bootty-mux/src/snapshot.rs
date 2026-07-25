#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MuxSnapshot {
    pub sessions: Vec<MuxSession>,
    pub active_session_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MuxSession {
    pub id: String,
    pub name: String,
    pub active: bool,
    pub anchor: MuxPaneAnchor,
    pub active_window_id: Option<String>,
    pub windows: Vec<MuxWindow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MuxPaneSplitDirection {
    Right,
    Down,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MuxPaneLayout {
    Pane(String),
    Split {
        direction: MuxPaneSplitDirection,
        ratio_millis: u16,
        first: Box<MuxPaneLayout>,
        second: Box<MuxPaneLayout>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MuxWindow {
    pub id: String,
    pub index: u32,
    pub name: String,
    pub active: bool,
    pub anchor: MuxPaneAnchor,
    /// Every pane in the window, in order. The native engine renders these as an egui split layout;
    /// other backends own their own layout and expose only the single attach anchor here.
    pub panes: Vec<MuxPaneAnchor>,
    /// Native-layout shape for backends that expose a durable split tree.
    pub layout: Option<MuxPaneLayout>,
    /// Progress the backend already tracks for this window, for backends that multiplex every
    /// pane over one attach PTY. They only forward the active pane's OSC 9;4, so asking the
    /// backend is the only way to see a background window's progress.
    pub progress: Option<MuxWindowProgress>,
}

/// Backend-reported progress, in the ConEmu vocabulary the OSC 9;4 parser already speaks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MuxWindowProgress {
    pub state: String,
    pub percent: Option<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MuxPaneAnchor {
    pub session_id: String,
    pub pane_id: Option<String>,
    pub cwd: Option<String>,
    pub process: Option<String>,
}

pub fn selection_after_refresh(current: Option<String>, snapshot: &MuxSnapshot) -> Option<String> {
    current
        .filter(|current| {
            snapshot
                .sessions
                .iter()
                .any(|session| session.id == *current || session.name == *current)
        })
        .or_else(|| {
            snapshot
                .sessions
                .iter()
                .find(|session| session.active)
                .or_else(|| snapshot.sessions.first())
                .map(|session| session.id.clone())
        })
}
