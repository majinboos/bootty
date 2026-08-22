use bootty_mux::snapshot::{SESSION_IDENTITY_OPTION, SESSION_SPACE_OPTION};
use bootty_rmux::{numeric_session_id, session_tag_option, tag_option_id};

/// rmux keys its option store by session name and does not migrate it on rename, so the tag hangs
/// off the session's stable id instead. Reading an option back means matching that id, and only
/// options bootty owns are ever matched.
#[test]
fn a_tag_option_is_matched_to_its_session_id_and_nothing_else() {
    assert_eq!(
        session_tag_option("$7", SESSION_IDENTITY_OPTION),
        "@bootty_id_7",
        "the id sigil buys nothing inside an option name"
    );
    assert_eq!(
        session_tag_option("$7", SESSION_SPACE_OPTION),
        "@bootty_space_7"
    );

    assert_eq!(tag_option_id("@bootty_id_3"), Some(3));
    assert_eq!(tag_option_id("@bootty_space_12"), Some(12));
    assert_eq!(tag_option_id("@bootty_id"), None);
    assert_eq!(
        tag_option_id("@someone_elses_option_3"),
        None,
        "another tool's server option is never mistaken for a stale tag"
    );
    assert_eq!(tag_option_id("@bootty_id_notanumber"), None);

    assert_eq!(numeric_session_id("$7"), Some(7));
    assert_eq!(numeric_session_id("7"), Some(7));
    assert_eq!(numeric_session_id("nonsense"), None);
}
