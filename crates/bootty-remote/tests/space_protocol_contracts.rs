use bootty_mux::command::MuxCommand;
use bootty_remote::space_protocol::{decode_command, encode_command};

#[test]
fn remote_space_payload_preserves_command_arguments() {
    let command = MuxCommand::RenameSession {
        session_id: "space ; $HOME".to_owned(),
        name: "work & play".to_owned(),
    };

    let payload = encode_command(&command).expect("encode command");
    let decoded = decode_command(&payload).expect("decode command");

    assert_eq!(decoded, command);
}
