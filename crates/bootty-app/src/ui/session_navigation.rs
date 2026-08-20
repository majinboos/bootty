use std::collections::HashMap;

use crate::mux::{controller::MuxScope, snapshot::MuxSession};

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct ScopedSessionTarget {
    pub scope: MuxScope,
    pub session_id: String,
}

impl ScopedSessionTarget {
    pub fn new(scope: MuxScope, session_id: impl Into<String>) -> Self {
        Self {
            scope,
            session_id: session_id.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindingSessionGroup {
    pub scope: MuxScope,
    pub label: String,
    pub sessions: Vec<MuxSession>,
    pub selected_session: Option<String>,
    pub active: bool,
    pub can_return_to_last_session: bool,
    /// What bootty calls each session, by session id, for the ones whose backend name carries a
    /// uniqueness suffix bootty never meant to show.
    pub display_names: HashMap<String, String>,
}

impl BindingSessionGroup {
    pub fn target(&self, session: &MuxSession) -> ScopedSessionTarget {
        ScopedSessionTarget::new(self.scope, session.id.clone())
    }

    /// The name to show for `session`: bootty's own, or the backend's when it has none.
    pub fn display_name<'a>(&'a self, session: &'a MuxSession) -> &'a str {
        self.display_names
            .get(&session.id)
            .map_or(session.name.as_str(), String::as_str)
    }

    pub fn session_is_current(&self, session: &MuxSession) -> bool {
        self.active
            && self
                .selected_session
                .as_deref()
                .map_or(session.active, |selected| {
                    selected == session.id.as_str() || selected == session.name.as_str()
                })
    }
}
