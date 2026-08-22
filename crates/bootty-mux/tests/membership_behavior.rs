use bootty_mux::membership::{BackendMembership, MembershipOperation};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use proptest_derive::Arbitrary;
use rstest::rstest;

fn membership(id: &str, name: &str, identity: Option<&str>) -> BackendMembership {
    BackendMembership {
        id: id.to_owned(),
        name: name.to_owned(),
        identity: identity.map(str::to_owned),
    }
}

#[rstest]
#[case(MembershipOperation::Create { identity: String::new(), session_name: "session".into() })]
#[case(MembershipOperation::Create { identity: "id".into(), session_name: "bad\0name".into() })]
#[case(MembershipOperation::Rename { identity: "id".into(), old_name: "same".into(), new_name: "same".into() })]
#[case(MembershipOperation::Ditch { identity: "id\0one".into(), old_name: "session".into() })]
fn invalid_operations_are_rejected(#[case] operation: MembershipOperation) {
    assert_eq!(
        operation.validate().map_err(|error| error.to_string()),
        Err("membership operation is invalid".to_owned())
    );
}

#[rstest]
#[case(
    MembershipOperation::Rename { identity: "id-1".into(), old_name: "before".into(), new_name: "after".into() },
    vec![membership("$4", "after", Some("id-1"))],
    true
)]
#[case(
    MembershipOperation::Rename { identity: "id-1".into(), old_name: "before".into(), new_name: "after".into() },
    vec![membership("$4", "after", Some("id-2"))],
    false
)]
#[case(
    MembershipOperation::Ditch { identity: "id-1".into(), old_name: "gone".into() },
    vec![membership("$4", "renamed", Some("id-1"))],
    false
)]
#[case(
    MembershipOperation::Ditch { identity: "id-1".into(), old_name: "gone".into() },
    vec![membership("$4", "gone", Some("id-2"))],
    true
)]
fn completion_is_decided_by_identity_and_the_requested_effect(
    #[case] operation: MembershipOperation,
    #[case] observed: Vec<BackendMembership>,
    #[case] expected: bool,
) {
    assert_eq!(operation.effect_occurred(&observed), expected);
}

#[derive(Arbitrary, Debug)]
struct ObservedMembership {
    name: String,
    identity: Option<String>,
}

#[derive(Arbitrary, Debug)]
struct CreateScenario {
    identity: String,
    requested_name: String,
    sessions: Vec<ObservedMembership>,
}

proptest! {
    /// Property: create completion is exactly identity membership, independent of session names.
    #[test]
    fn create_completion_matches_identity_presence(scenario in any::<CreateScenario>()) {
        let observed = scenario.sessions
            .into_iter()
            .enumerate()
            .map(|(index, session)| BackendMembership {
                id: format!("${index}"),
                name: session.name,
                identity: session.identity,
            })
            .collect::<Vec<_>>();
        let expected = observed
            .iter()
            .any(|session| session.identity.as_deref() == Some(scenario.identity.as_str()));
        let operation = MembershipOperation::Create {
            identity: scenario.identity,
            session_name: scenario.requested_name,
        };

        prop_assert_eq!(operation.effect_occurred(&observed), expected);
    }
}
