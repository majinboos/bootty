use bootty_mux::membership::{BackendMembership, MembershipOperation};

fn membership(id: &str, name: &str) -> BackendMembership {
    BackendMembership {
        id: id.to_owned(),
        name: name.to_owned(),
    }
}

#[test]
fn membership_operations_reject_invalid_backend_facts() {
    for invalid in [
        MembershipOperation::Create {
            session_id: String::new(),
            session_name: "session".to_owned(),
        },
        MembershipOperation::Rename {
            session_id: "session".to_owned(),
            old_name: "same".to_owned(),
            new_name: "same".to_owned(),
        },
        MembershipOperation::Ditch {
            session_id: "session\0id".to_owned(),
            old_name: "session".to_owned(),
        },
    ] {
        assert_eq!(
            invalid.validate().unwrap_err().to_string(),
            "membership operation is invalid"
        );
    }
}

#[test]
fn authoritative_membership_classifies_each_backend_identity_model() {
    let create = MembershipOperation::Create {
        session_id: "stable-1".to_owned(),
        session_name: "created".to_owned(),
    };
    assert!(create.effect_occurred(&[membership("stable-1", "created")]));
    assert!(create.effect_occurred(&[membership("created", "created")]));

    let rename = MembershipOperation::Rename {
        session_id: "stable-1".to_owned(),
        old_name: "before".to_owned(),
        new_name: "after".to_owned(),
    };
    assert!(rename.effect_occurred(&[membership("stable-1", "after")]));
    assert!(rename.effect_occurred(&[membership("after", "after")]));
    assert!(
        !rename.effect_occurred(&[membership("before", "before"), membership("other", "after"),])
    );

    let ditch = MembershipOperation::Ditch {
        session_id: "stable-1".to_owned(),
        old_name: "after".to_owned(),
    };
    assert!(ditch.effect_occurred(&[membership("other", "other")]));
    assert!(!ditch.effect_occurred(&[membership("stable-1", "after")]));
}
