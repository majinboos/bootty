use bootty_mux::membership::{BackendMembership, MembershipOperation};

fn membership(id: &str, name: &str, identity: Option<&str>) -> BackendMembership {
    BackendMembership {
        id: id.to_owned(),
        name: name.to_owned(),
        identity: identity.map(str::to_owned),
    }
}

#[test]
fn membership_operations_reject_invalid_backend_facts() {
    for invalid in [
        MembershipOperation::Create {
            identity: String::new(),
            session_name: "session".to_owned(),
        },
        MembershipOperation::Rename {
            identity: "id-1".to_owned(),
            old_name: "same".to_owned(),
            new_name: "same".to_owned(),
        },
        MembershipOperation::Ditch {
            identity: "id\u{0}1".to_owned(),
            old_name: "session".to_owned(),
        },
    ] {
        assert_eq!(
            invalid.validate().unwrap_err().to_string(),
            "membership operation is invalid"
        );
    }
}

/// An operation is settled by the identity bootty stamped onto the session, never by the name it
/// happened to have. A backend that renamed the session, or handed the old name to someone else,
/// cannot change the answer.
#[test]
fn an_operation_is_settled_by_the_identity_and_not_by_any_name() {
    let create = MembershipOperation::Create {
        identity: "id-1".to_owned(),
        session_name: "created".to_owned(),
    };
    assert!(create.effect_occurred(&[membership("$4", "created-2", Some("id-1"))]));
    assert!(!create.effect_occurred(&[membership("$4", "created", None)]));

    let rename = MembershipOperation::Rename {
        identity: "id-1".to_owned(),
        old_name: "before".to_owned(),
        new_name: "after".to_owned(),
    };
    assert!(rename.effect_occurred(&[membership("$4", "after", Some("id-1"))]));
    assert!(!rename.effect_occurred(&[membership("$4", "after", Some("id-2"))]));

    let ditch = MembershipOperation::Ditch {
        identity: "id-1".to_owned(),
        old_name: "after".to_owned(),
    };
    assert!(ditch.effect_occurred(&[membership("$4", "after", Some("id-2"))]));
    assert!(!ditch.effect_occurred(&[membership("$4", "renamed", Some("id-1"))]));
}

/// A create is answered by the identity, so a session of the same name that bootty did not make
/// cannot be mistaken for the one it asked for.
#[test]
fn someone_elses_session_of_the_same_name_is_not_this_create_landing() {
    let create = MembershipOperation::Create {
        identity: "id-1".to_owned(),
        session_name: "agents/main".to_owned(),
    };
    assert!(!create.effect_occurred(&[membership("$4", "agents/main", None)]));
    assert!(create.effect_occurred(&[membership("$4", "agents/main", Some("id-1"))]));
}

/// A ditch only holds once nothing carries the identity: a session that was renamed rather than
/// killed still carries it.
#[test]
fn a_ditch_is_not_settled_by_a_session_that_was_only_renamed() {
    let ditch = MembershipOperation::Ditch {
        identity: "id-1".to_owned(),
        old_name: "gone".to_owned(),
    };
    assert!(!ditch.effect_occurred(&[membership("$4", "renamed-not-killed", Some("id-1"))]));
    assert!(ditch.effect_occurred(&[membership("$4", "gone", Some("id-2"))]));
}
