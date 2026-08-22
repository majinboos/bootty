use bootty_mux_model::{MuxBackendKind, MuxBindingConfig, RemoteSpaceSummary, SshTarget};

#[test]
fn backend_tokens_are_stable() {
    for (token, expected) in [
        ("native", MuxBackendKind::Native),
        ("rmux", MuxBackendKind::Rmux),
        ("tmux", MuxBackendKind::Tmux),
    ] {
        let decoded: MuxBackendKind = serde_json::from_str(&format!("\"{token}\""))
            .expect("backend token should deserialize");
        assert_eq!(decoded, expected);
    }
}

#[test]
fn remote_space_summary_wire_shape_is_stable() {
    for (backend, token) in [
        (MuxBackendKind::Rmux, "rmux"),
        (MuxBackendKind::Tmux, "tmux"),
    ] {
        let summary = RemoteSpaceSummary {
            catalog_version: 3,
            id: "space-id".to_owned(),
            name: "Lab".to_owned(),
            backend,
        };
        let encoded = serde_json::to_string(&summary).expect("encode remote Space summary");
        assert_eq!(
            encoded,
            format!(r#"{{"catalog_version":3,"id":"space-id","name":"Lab","backend":"{token}"}}"#)
        );
        assert_eq!(
            serde_json::from_str::<RemoteSpaceSummary>(&encoded)
                .expect("decode remote Space summary"),
            summary
        );
    }
}

#[test]
fn ssh_target_preserves_fields_and_defaults() {
    let defaults: SshTarget = serde_json::from_str(r#"{"host":"devbox"}"#)
        .expect("host-only SSH target should deserialize");
    assert_eq!(defaults, SshTarget::for_host("devbox"));

    let explicit: SshTarget = serde_json::from_str(
        r#"{"host":"10.0.0.4","user":"dev","port":2222,"program":"ssh-custom","args":["-i","key"]}"#,
    )
    .expect("all SSH target fields should deserialize");
    assert_eq!(explicit.host, "10.0.0.4");
    assert_eq!(explicit.user.as_deref(), Some("dev"));
    assert_eq!(explicit.port, Some(2222));
    assert_eq!(explicit.program, "ssh-custom");
    assert_eq!(explicit.args, ["-i", "key"]);
}

#[test]
fn binding_defaults_and_remote_validation_are_stable() {
    let defaults = MuxBindingConfig::default();
    assert_eq!(defaults.backend, MuxBackendKind::Native);
    assert!(!defaults.hide_tmux_status);
    assert_eq!(defaults.remote, None);
    assert_eq!(defaults.remote_space_id, None);

    let empty_host = MuxBindingConfig {
        remote: Some(SshTarget::for_host("  ")),
        ..defaults.clone()
    };
    assert_eq!(
        empty_host.validate_remote().unwrap_err().to_string(),
        "multiplexer.remote.host must name a host"
    );

    let unsupported = MuxBindingConfig {
        backend: MuxBackendKind::Native,
        remote: Some(SshTarget::for_host("devbox")),
        ..defaults
    };
    assert_eq!(
        unsupported.validate_remote().unwrap_err().to_string(),
        "multiplexer.remote needs a backend with a client to run there, got Native"
    );
}
