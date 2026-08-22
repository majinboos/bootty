use std::collections::HashSet;

use bootty_workspace::{SessionMembership, WorkspaceSession};

fn session(identity: &str, name: &str) -> WorkspaceSession {
    WorkspaceSession {
        identity: identity.to_owned(),
        backend_name: name.to_owned(),
        display_name: String::new(),
        explicit: false,
        cwd: "/repo".to_owned(),
    }
}

fn labels(membership: &SessionMembership) -> Vec<&str> {
    membership
        .sessions()
        .iter()
        .map(WorkspaceSession::label)
        .collect()
}

/// The point of the whole redesign: a rename is not a membership change. The backend hands out
/// a new name, the claim does not move, and the display name is untouched.
#[test]
fn a_rename_from_anywhere_leaves_the_claim_and_the_display_name_alone() {
    let mut membership = SessionMembership::default();
    membership.claim(session("id-1", "agents/main"));
    membership.set_display_name("id-1", "agents/main", true);

    assert!(membership.observe_backend_name("id-1", "renamed-elsewhere"));

    let claimed = membership.get("id-1").expect("the claim survives a rename");
    assert_eq!(claimed.backend_name, "renamed-elsewhere");
    assert_eq!(claimed.label(), "agents/main");
}

/// Two Spaces on one server can both hold what bootty calls `agents/main`; only the backend
/// name has to be unique, and it is not what anything is keyed on.
#[test]
fn two_sessions_can_share_a_display_name_when_the_backend_had_to_uniquify_one() {
    let mut membership = SessionMembership::default();
    membership.claim(session("id-1", "agents/main"));
    membership.claim(session("id-2", "agents/main-2"));
    membership.set_display_name("id-1", "agents/main", true);
    membership.set_display_name("id-2", "agents/main", true);

    assert_eq!(labels(&membership), ["agents/main", "agents/main"]);
    assert_eq!(
        membership.backend_names(),
        ["agents/main", "agents/main-2"],
        "the backend keeps the names it needs to tell them apart"
    );
}

#[test]
fn a_claimed_session_joins_its_group_rather_than_the_end_of_the_list() {
    let mut membership = SessionMembership::default();
    membership.claim(session("id-1", "agents/main"));
    membership.claim(session("id-2", "web/dev"));
    membership.claim(session("id-3", "agents/review"));

    assert_eq!(
        labels(&membership),
        ["agents/main", "agents/review", "web/dev"]
    );
}

#[test]
fn a_session_reorders_inside_its_group_and_carries_the_group_across_one() {
    let mut membership = SessionMembership::default();
    for (identity, name) in [
        ("id-1", "agents/main"),
        ("id-2", "agents/review"),
        ("id-3", "web/dev"),
    ] {
        membership.claim(session(identity, name));
    }

    assert!(membership.move_before("id-2", Some("id-1")));
    assert_eq!(
        labels(&membership),
        ["agents/review", "agents/main", "web/dev"]
    );

    assert!(
        !membership.move_before("id-1", Some("id-3")),
        "the agents block already sits before web/dev"
    );
    assert!(membership.move_before("id-3", Some("id-1")));
    assert_eq!(
        labels(&membership),
        ["web/dev", "agents/review", "agents/main"],
        "a session cannot leave its group, so the whole group travels"
    );
}

/// An empty snapshot is a backend that has not answered yet, not a Space that emptied.
#[test]
fn pruning_ignores_an_empty_snapshot_and_drops_sessions_that_really_went_away() {
    let mut membership = SessionMembership::default();
    membership.claim(session("id-1", "one"));
    membership.claim(session("id-2", "two"));

    assert!(!membership.retain_alive(&HashSet::new()));
    assert_eq!(membership.sessions().len(), 2);

    assert!(membership.retain_alive(&HashSet::from(["id-1"])));
    assert_eq!(labels(&membership), ["one"]);
}

#[test]
fn releasing_a_session_hands_it_back_so_another_space_can_claim_it() {
    let mut membership = SessionMembership::default();
    membership.claim(session("id-1", "one"));

    let released = membership.release("id-1").expect("the claimed session");
    assert_eq!(released.backend_name, "one");
    assert!(membership.is_empty());
    assert!(membership.release("id-1").is_none());
}
