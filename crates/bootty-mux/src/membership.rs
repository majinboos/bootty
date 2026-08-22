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
