use bootty_mux::command::MuxCommand;

#[test]
fn remote_space_payload_preserves_command_arguments() {
    let command = MuxCommand::RenameSession {
        session_id: "space ; $HOME".to_owned(),
        name: "work & play".to_owned(),
    };

    let payload = bootty_mux::encode_remote_space_command(&command).expect("encode command");
    let decoded = bootty_mux::decode_remote_space_command(&payload).expect("decode command");

    assert_eq!(decoded, command);
}
