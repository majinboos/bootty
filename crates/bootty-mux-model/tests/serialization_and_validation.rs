use bootty_mux_model::{MuxBackendKind, MuxBindingConfig, RemoteSpaceSummary, SshTarget};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use proptest_derive::Arbitrary;
use rstest::rstest;

const EMPTY_HOST: &str = "multiplexer.remote.host must name a host";
const LOCAL_REMOTE: &str =
    "multiplexer.remote needs a backend with a client to run there, got Native";

#[test]
fn remote_space_summary_uses_the_documented_wire_shape() {
    for (backend, token) in [
        (MuxBackendKind::Native, "native"),
        (MuxBackendKind::Rmux, "rmux"),
        (MuxBackendKind::Tmux, "tmux"),
    ] {
        let summary = RemoteSpaceSummary {
            catalog_version: 3,
            id: "space-id".to_owned(),
            name: "Lab".to_owned(),
            backend,
        };
        let actual = serde_json::to_value(&summary).expect("encode remote Space summary");
        let expected = serde_json::json!({
            "catalog_version": 3, "id": "space-id", "name": "Lab", "backend": token,
        });
        assert_eq!(actual, expected);
        let decoded: RemoteSpaceSummary = serde_json::from_value(actual).unwrap();
        assert_eq!(decoded, summary);
    }
}

#[derive(Arbitrary, Clone, Debug)]
struct SshTargetInput(String, Option<String>, Option<u16>, String, Vec<String>);

proptest! {
    /// Property: every SSH target field survives a JSON round trip, including arbitrary Unicode.
    #[test]
    fn ssh_target_json_round_trip_preserves_all_fields(input in any::<SshTargetInput>()) {
        let SshTargetInput(host, user, port, program, args) = input;
        let target = SshTarget {
            host: host.clone(), user: user.clone(), port, program: program.clone(), args: args.clone(),
        };
        let expected = serde_json::json!({
            "host": host, "user": user, "port": port, "program": program, "args": args,
        });
        let encoded = serde_json::to_value(&target).expect("serialize SSH target");
        assert_eq!(encoded, expected);
        prop_assert_eq!(serde_json::from_value::<SshTarget>(encoded).unwrap(), target);
    }
}

#[test]
fn host_only_ssh_target_receives_process_defaults() {
    let actual: SshTarget =
        serde_json::from_str(r#"{"host":"devbox"}"#).expect("deserialize host-only target");

    assert_eq!(actual, SshTarget::for_host("devbox"));
}

#[rstest]
#[case(MuxBackendKind::Native, None, None)]
#[case(MuxBackendKind::Rmux, Some("devbox"), None)]
#[case(MuxBackendKind::Tmux, Some("devbox"), None)]
#[case(MuxBackendKind::Tmux, Some("  "), Some(EMPTY_HOST))]
#[case(MuxBackendKind::Native, Some("devbox"), Some(LOCAL_REMOTE))]
fn remote_validation_depends_on_host_and_backend_support(
    #[case] backend: MuxBackendKind,
    #[case] host: Option<&str>,
    #[case] expected_error: Option<&str>,
) {
    let config = MuxBindingConfig {
        backend,
        remote: host.map(SshTarget::for_host),
        ..MuxBindingConfig::default()
    };
    let error = config
        .validate_remote()
        .err()
        .map(|error| error.to_string());
    assert_eq!(error.as_deref(), expected_error);
}
