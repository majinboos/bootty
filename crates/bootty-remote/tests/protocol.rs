use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bootty_mux::command::MuxCommand;
use bootty_remote::{
    shell_quote,
    space_protocol::{decode_command, encode_command},
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use proptest_derive::Arbitrary;
use serde_json::json;
use static_assertions::assert_impl_all;

assert_impl_all!(MuxCommand: Send, Sync);

#[derive(Arbitrary, Debug)]
struct RenameCommand {
    #[proptest(regex = ".{0,128}")]
    session_id: String,
    #[proptest(regex = ".{0,128}")]
    name: String,
}

proptest! {
    /// Property: POSIX single quoting preserves every scalar and escapes only apostrophes.
    #[test]
    fn shell_quoting_matches_the_posix_oracle(value in ".{0,256}") {
        assert_eq!(shell_quote(&value), format!("'{}'", value.replace('\'', "'\\''")));
    }

    /// Property: the wire representation and its decoded command preserve arbitrary arguments.
    #[test]
    fn space_commands_match_the_wire_oracle(model in any::<RenameCommand>()) {
        let RenameCommand { session_id, name } = model;
        let command = MuxCommand::RenameSession { session_id: session_id.clone(), name: name.clone() };
        let encoded = encode_command(&command).expect("encode command");
        let wire: serde_json::Value = serde_json::from_slice(
            &URL_SAFE_NO_PAD.decode(&encoded).expect("decode base64"),
        ).expect("decode JSON");
        assert_eq!(wire, json!({ "RenameSession": { "session_id": session_id, "name": name } }));
        assert_eq!(decode_command(&encoded).expect("decode command"), command);
    }
}
