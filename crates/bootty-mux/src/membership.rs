use thiserror::Error;

/// A backend session membership observed in a multiplexer snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendMembership {
    pub id: String,
    pub name: String,
    /// The identity the session carries. `None` is a session bootty has never claimed, or one
    /// whose server restarted and dropped every tag.
    pub identity: Option<String>,
}

impl BackendMembership {
    fn carries(&self, identity: &str) -> bool {
        self.identity.as_deref() == Some(identity)
    }
}

/// A durable membership change requested from a multiplexer backend.
///
/// Keyed by the identity bootty stamped into the session, so "did it happen" is a lookup rather
/// than a guess assembled from names that may have moved on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MembershipOperation {
    Create {
        identity: String,
        session_name: String,
    },
    Rename {
        identity: String,
        old_name: String,
        new_name: String,
    },
    Ditch {
        identity: String,
        old_name: String,
    },
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("membership operation is invalid")]
pub struct MembershipValidationError;

impl MembershipOperation {
    pub fn identity(&self) -> &str {
        match self {
            Self::Create { identity, .. }
            | Self::Rename { identity, .. }
            | Self::Ditch { identity, .. } => identity,
        }
    }

    /// Reject empty or NUL-containing backend identity values.
    pub fn validate(&self) -> Result<(), MembershipValidationError> {
        let valid = |value: &str| !value.is_empty() && !value.contains('\0');
        let valid = valid(self.identity())
            && match self {
                Self::Create { session_name, .. } => valid(session_name),
                Self::Rename {
                    old_name, new_name, ..
                } => valid(old_name) && valid(new_name) && old_name != new_name,
                Self::Ditch { old_name, .. } => valid(old_name),
            };
        valid.then_some(()).ok_or(MembershipValidationError)
    }

    /// Return whether a fresh backend snapshot proves that this operation occurred.
    pub fn effect_occurred(&self, memberships: &[BackendMembership]) -> bool {
        match self {
            Self::Create { identity, .. } => {
                memberships.iter().any(|session| session.carries(identity))
            }
            Self::Rename {
                identity, new_name, ..
            } => memberships
                .iter()
                .any(|session| session.carries(identity) && session.name == *new_name),
            Self::Ditch { identity, .. } => {
                !memberships.iter().any(|session| session.carries(identity))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn membership(id: &str, name: &str, identity: Option<&str>) -> BackendMembership {
        BackendMembership {
            id: id.to_owned(),
            name: name.to_owned(),
            identity: identity.map(str::to_owned),
        }
    }

    /// The reason the identity is stamped at all: after a create whose result never came back,
    /// the answer is in the session, not in a name that another Space may have taken meanwhile.
    #[test]
    fn a_create_is_settled_by_the_identity_rather_than_by_the_name_it_asked_for() {
        let operation = MembershipOperation::Create {
            identity: "id-1".to_owned(),
            session_name: "agents/main".to_owned(),
        };

        assert!(operation.effect_occurred(&[membership("$4", "agents/main-2", Some("id-1"))]));
        assert!(
            !operation.effect_occurred(&[membership("$4", "agents/main", None)]),
            "someone else's session of the same name is not this create landing"
        );
    }

    #[test]
    fn a_rename_needs_the_new_name_on_the_session_that_carries_the_identity() {
        let operation = MembershipOperation::Rename {
            identity: "id-1".to_owned(),
            old_name: "before".to_owned(),
            new_name: "after".to_owned(),
        };

        assert!(operation.effect_occurred(&[membership("$4", "after", Some("id-1"))]));
        assert!(!operation.effect_occurred(&[membership("$4", "before", Some("id-1"))]));
        assert!(!operation.effect_occurred(&[membership("$4", "after", Some("id-2"))]));
    }

    #[test]
    fn a_ditch_holds_only_once_nothing_carries_the_identity_any_more() {
        let operation = MembershipOperation::Ditch {
            identity: "id-1".to_owned(),
            old_name: "gone".to_owned(),
        };

        assert!(operation.effect_occurred(&[membership("$4", "gone", Some("id-2"))]));
        assert!(!operation.effect_occurred(&[membership(
            "$4",
            "renamed-not-killed",
            Some("id-1")
        )]));
    }
}
