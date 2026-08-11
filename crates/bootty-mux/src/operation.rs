use serde::{Deserialize, Serialize};

/// An opaque identity for the process currently occupying a pane.
///
/// `backend_identity` is authoritative and must change when an occupant is replaced, even when
/// the backend happens to reuse the pane id, PID, and process text.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct MuxOccupantIdentity {
    pub backend_identity: String,
    pub pid: Option<u32>,
    pub process: Option<String>,
}

/// A backend resource addressed by exact backend IDs.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct MuxEventTarget {
    pub session_id: Option<String>,
    pub window_id: Option<String>,
    pub pane_id: Option<String>,
    pub terminal_id: Option<String>,
    pub occupant: Option<MuxOccupantIdentity>,
}

impl MuxEventTarget {
    pub fn session(session_id: impl Into<String>) -> Self {
        Self {
            session_id: Some(session_id.into()),
            ..Self::default()
        }
    }

    pub fn pane(
        session_id: impl Into<String>,
        window_id: impl Into<String>,
        pane_id: impl Into<String>,
        terminal_id: impl Into<String>,
        occupant: Option<MuxOccupantIdentity>,
    ) -> Self {
        Self {
            session_id: Some(session_id.into()),
            window_id: Some(window_id.into()),
            pane_id: Some(pane_id.into()),
            terminal_id: Some(terminal_id.into()),
            occupant,
        }
    }
}

/// A backend-owned failure classification retained across local and remote operation seams.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MuxBackendOperationError {
    Unsupported(String),
    Unavailable(String),
    Denied(String),
    Stale(String),
    Failed(String),
}

impl MuxBackendOperationError {
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported(message.into())
    }

    pub fn stale(message: impl Into<String>) -> Self {
        Self::Stale(message.into())
    }
}

impl std::fmt::Display for MuxBackendOperationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::Unsupported(message)
            | Self::Unavailable(message)
            | Self::Denied(message)
            | Self::Stale(message)
            | Self::Failed(message) => message,
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for MuxBackendOperationError {}

/// Exact backend IDs allocated by a recursive session launch.
///
/// `pane_ids` always follows the launch layout's DFS declaration order, not backend creation order.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct MuxAllocatedResources {
    pub session_id: String,
    pub windows: Vec<MuxAllocatedWindow>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct MuxAllocatedWindow {
    pub window_id: String,
    pub pane_ids: Vec<String>,
}

/// Authoritative backend facts retained after a mutation and consumed with its completion.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct MuxBackendCommandCompletion {
    pub allocated: Option<MuxAllocatedResources>,
    pub target: Option<MuxEventTarget>,
}
