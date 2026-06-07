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
pub struct MuxWindow {
    pub id: String,
    pub index: u32,
    pub name: String,
    pub active: bool,
    pub anchor: MuxPaneAnchor,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MuxPaneAnchor {
    pub session_id: String,
    pub pane_id: Option<String>,
    pub cwd: Option<String>,
    pub process: Option<String>,
}
