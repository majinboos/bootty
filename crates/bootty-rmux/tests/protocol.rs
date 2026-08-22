use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bootty_mux::{
    command::{MuxCommand, MuxSplitDirection},
    snapshot::{SESSION_IDENTITY_OPTION, SESSION_SPACE_OPTION},
};
use bootty_rmux::{RemoteRmuxRequest, numeric_session_id, session_tag_option, tag_option_id};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use proptest_derive::Arbitrary;
use rstest::rstest;
use serde_json::{Value, json};
use static_assertions::assert_impl_all;

assert_impl_all!(RemoteRmuxRequest: Send, Sync);

#[derive(Arbitrary, Debug)]
struct RequestCase {
    #[proptest(regex = ".{0,128}")]
    session: String,
    #[proptest(regex = ".{0,128}")]
    pane: String,
    pane_is_some: bool,
    down: bool,
    cols: u16,
    rows: u16,
}

proptest! {
    /// Property: every request matches the independent JSON oracle and round-trips losslessly.
    #[test]
    fn requests_match_the_wire_oracle(case in any::<RequestCase>()) {
        let RequestCase { session, pane, pane_is_some, down, cols, rows } = case;
        let direction = if down { MuxSplitDirection::Down } else { MuxSplitDirection::Right };
        let direction_name = if down { "Down" } else { "Right" };
        let pane_id = pane_is_some.then(|| pane.clone());
        let cases = [
            (
                RemoteRmuxRequest::Execute { command: MuxCommand::SplitPane {
                    session_id: session.clone(), pane_id: pane_id.clone(), direction,
                } },
                json!({ "Execute": { "command": { "SplitPane": {
                    "session_id": session.clone(), "pane_id": pane_id.clone(), "direction": direction_name,
                } } } }),
            ),
            (RemoteRmuxRequest::PaneStream { session: session.clone(), pane: pane.clone() },
             json!({ "PaneStream": { "session": session.clone(), "pane": pane.clone() } })),
            (RemoteRmuxRequest::PaneInput { session: session.clone(), pane: pane.clone() },
             json!({ "PaneInput": { "session": session.clone(), "pane": pane.clone() } })),
            (RemoteRmuxRequest::Resize { session: session.clone(), pane: pane.clone(), cols, rows },
             json!({ "Resize": { "session": session, "pane": pane, "cols": cols, "rows": rows } })),
        ];
        for (request, expected) in cases {
            let payload = request.encode().expect("encode request");
            let wire: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&payload).expect("base64"))
                .expect("JSON");
            assert_eq!(wire, expected);
            assert_eq!(RemoteRmuxRequest::decode(&payload).expect("decode request"), request);
            prop_assert!(payload.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')));
        }
    }

    /// Property: Bootty's rmux option names recover exactly the stable numeric session id.
    #[test]
    fn owned_tag_options_round_trip(id in any::<u32>(), space in any::<bool>()) {
        let option = if space { SESSION_SPACE_OPTION } else { SESSION_IDENTITY_OPTION };
        let session = format!("${id}");
        let tag = session_tag_option(&session, option);
        assert_eq!(tag, format!("{option}_{id}"));
        assert_eq!((tag_option_id(&tag), numeric_session_id(&session)), (Some(id), Some(id)));
        assert_eq!(numeric_session_id(&id.to_string()), Some(id));
    }
}

#[rstest]
#[case("not-base64!".to_owned())]
#[case("A".repeat(2 * 1024 * 1024 + 1))]
fn invalid_payloads_are_rejected(#[case] payload: String) {
    assert!(RemoteRmuxRequest::decode(&payload).is_err());
}

#[test]
fn unowned_or_malformed_identifiers_are_rejected() {
    for option in [
        "@bootty_id",
        "@someone_elses_option_3",
        "@bootty_id_notanumber",
    ] {
        assert_eq!(tag_option_id(option), None);
    }
    assert_eq!(numeric_session_id("nonsense"), None);
}
