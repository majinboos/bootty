use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub enum MuxSnapshotDisposition {
    #[default]
    Authoritative,
    Transient,
}

impl MuxSnapshotDisposition {
    pub fn is_authoritative(&self) -> bool {
        matches!(self, Self::Authoritative)
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct MuxSnapshot {
    pub sessions: Vec<MuxSession>,
    pub active_session_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "MuxSnapshotDisposition::is_authoritative"
    )]
    pub disposition: MuxSnapshotDisposition,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct MuxSession {
    pub id: String,
    pub name: String,
    pub active: bool,
    pub anchor: MuxPaneAnchor,
    pub active_window_id: Option<String>,
    pub windows: Vec<MuxWindow>,
    #[serde(default, skip_serializing_if = "MuxSessionTag::is_empty")]
    pub tag: MuxSessionTag,
}

/// Bootty's handle on a session, stored in the multiplexer rather than in bootty.
///
/// Names belong to whoever is looking at them, so nothing bootty persists may key on one. Empty
/// means the session belongs to no Space yet -- made outside bootty, or its server restarted.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct MuxSessionTag {
    /// The durable id bootty minted for this session, stable across every rename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    /// The Space that claims it, as that Space's portable id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<String>,
}

impl MuxSessionTag {
    pub fn is_empty(&self) -> bool {
        self.identity.is_none() && self.space.is_none()
    }
}

/// The multiplexer option holding [`MuxSessionTag::identity`].
///
/// tmux hangs it off the session itself. rmux keys its option store by session name and does not
/// migrate it on rename, so the rmux backend keys the same option on the session's stable id at
/// server scope instead. Either way the tag lives in the multiplexer and every bootty window
/// looking at that server reads the same answer.
pub const SESSION_IDENTITY_OPTION: &str = "@bootty_id";

/// The multiplexer option holding [`MuxSessionTag::space`]. See [`SESSION_IDENTITY_OPTION`].
pub const SESSION_SPACE_OPTION: &str = "@bootty_space";

/// Mints a session identity. Random, because two bootty installs can share one server.
pub fn new_session_identity() -> String {
    let mut bytes = [0_u8; 16];
    // A failure here would leave the session untagged, which reads as "belongs to no Space" and is
    // recoverable, so it is not worth failing a session creation over.
    if getrandom::fill(&mut bytes).is_err() {
        return String::new();
    }
    let digits = b"0123456789abcdef";
    let mut identity = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        identity.push(digits[(byte >> 4) as usize] as char);
        identity.push(digits[(byte & 0x0f) as usize] as char);
    }
    identity
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum MuxPaneSplitDirection {
    Right,
    Down,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum MuxPaneLayout {
    Pane(String),
    Split {
        direction: MuxPaneSplitDirection,
        ratio_millis: u16,
        first: Box<MuxPaneLayout>,
        second: Box<MuxPaneLayout>,
    },
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
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
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct MuxWindowProgress {
    pub state: String,
    pub percent: Option<u8>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct MuxPaneAnchor {
    pub session_id: String,
    pub pane_id: Option<String>,
    /// The pane's process id, when the backend reports one. Lets a module walk the pane's process
    /// tree without asking the backend again for what a snapshot already knows.
    pub pane_pid: Option<u32>,
    pub cwd: Option<String>,
    pub process: Option<String>,
}

pub fn session_matches(session: &MuxSession, session_id: &str) -> bool {
    session.id == session_id || session.name == session_id
}

/// Resolves the selection against the sessions the backend reports, as that backend's session id.
///
/// Answering with the id rather than the string that came in is what keeps a selection stable: a name
/// stops resolving the moment the session is renamed, and the UI marks the current row by id, so a
/// name-tracked selection leaves the focused session unhighlighted.
pub fn selection_after_refresh(current: Option<String>, sessions: &[MuxSession]) -> Option<String> {
    current
        .and_then(|current| {
            sessions
                .iter()
                .find(|session| session_matches(session, &current))
                .map(|session| session.id.clone())
        })
        .or_else(|| {
            sessions
                .iter()
                .find(|session| session.active)
                .or_else(|| sessions.first())
                .map(|session| session.id.clone())
        })
}
