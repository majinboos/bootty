use std::{error::Error, fmt};

/// A backend session membership observed in a multiplexer snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendMembership {
    pub id: String,
    pub name: String,
}

/// A durable membership change requested from a multiplexer backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MembershipOperation {
    Create {
        session_id: String,
        session_name: String,
    },
    Rename {
        session_id: String,
        old_name: String,
        new_name: String,
    },
    Ditch {
        session_id: String,
        old_name: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MembershipValidationError;

impl fmt::Display for MembershipValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("membership operation is invalid")
    }
}

impl Error for MembershipValidationError {}

impl MembershipOperation {
    /// Reject empty or NUL-containing backend identity values.
    pub fn validate(&self) -> Result<(), MembershipValidationError> {
        let valid = |value: &str| !value.is_empty() && !value.contains('\0');
        let valid = match self {
            Self::Create {
                session_id,
                session_name,
            } => valid(session_id) && valid(session_name),
            Self::Rename {
                session_id,
                old_name,
                new_name,
            } => valid(session_id) && valid(old_name) && valid(new_name) && old_name != new_name,
            Self::Ditch {
                session_id,
                old_name,
            } => valid(session_id) && valid(old_name),
        };
        valid.then_some(()).ok_or(MembershipValidationError)
    }

    /// Return whether a fresh backend snapshot proves that this operation occurred.
    pub fn effect_occurred(&self, memberships: &[BackendMembership]) -> bool {
        match self {
            Self::Create {
                session_id,
                session_name,
            } => memberships
                .iter()
                .any(|session| session.id == *session_id || session.name == *session_name),
            Self::Rename {
                session_id,
                old_name,
                new_name,
            } => {
                let renamed_stable_id = memberships
                    .iter()
                    .any(|session| session.id == *session_id && session.name == *new_name);
                let renamed_name_key = memberships
                    .iter()
                    .any(|session| session.id == *new_name && session.name == *new_name)
                    && !memberships
                        .iter()
                        .any(|session| session.id == *old_name || session.name == *old_name);
                renamed_stable_id || renamed_name_key
            }
            Self::Ditch {
                session_id,
                old_name,
            } => !memberships.iter().any(|session| {
                session.id == *session_id
                    || session.name == *old_name
                    || session.name == *session_id
            }),
        }
    }
}
