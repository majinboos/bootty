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
}

impl BindingSessionGroup {
    pub fn target(&self, session: &MuxSession) -> ScopedSessionTarget {
        ScopedSessionTarget::new(self.scope, session.id.clone())
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

#[cfg(test)]
mod tests {
    use crate::mux::{
        controller::{BindingId, SpaceId},
        snapshot::MuxPaneAnchor,
    };

    use super::*;

    fn scope(binding_id: i64) -> MuxScope {
        MuxScope::new(
            SpaceId::from_persistence(1),
            BindingId::from_persistence(binding_id),
        )
    }

    fn session(id: &str, name: &str) -> MuxSession {
        MuxSession {
            id: id.to_owned(),
            name: name.to_owned(),
            active: false,
            anchor: MuxPaneAnchor {
                session_id: id.to_owned(),
                ..Default::default()
            },
            active_window_id: None,
            windows: Vec::new(),
        }
    }

    #[test]
    fn colliding_native_session_ids_remain_distinct_navigation_targets() {
        let local = BindingSessionGroup {
            scope: scope(10),
            label: "Local".to_owned(),
            sessions: vec![session("$1", "work")],
            selected_session: Some("$1".to_owned()),
            active: true,
            can_return_to_last_session: false,
        };
        let remote = BindingSessionGroup {
            scope: scope(20),
            label: "Remote".to_owned(),
            sessions: vec![session("$1", "work")],
            selected_session: Some("$1".to_owned()),
            active: false,
            can_return_to_last_session: false,
        };

        assert_ne!(
            local.target(&local.sessions[0]),
            remote.target(&remote.sessions[0])
        );
        assert!(local.session_is_current(&local.sessions[0]));
        assert!(!remote.session_is_current(&remote.sessions[0]));
    }
}
