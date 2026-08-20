use std::collections::BTreeMap;

use crate::{
    config::{
        MultiplexerBackendConfig, MultiplexerConfig, SshAuthenticationConfig,
        SshHostKeyPolicyConfig, SshProfileConfig, SshRemoteConfig,
    },
    workspace::{RemoteSpaceRef, SpaceRemoteOverride},
};

use super::mux_config::realize_binding_from;

fn product() -> MultiplexerConfig {
    MultiplexerConfig {
        backend: MultiplexerBackendConfig::Tmux,
        hide_tmux_status: true,
        remote: Some(SshRemoteConfig::for_host("product-host")),
        remote_space_id: Some("product-space".to_owned()),
    }
}

fn profile() -> SshProfileConfig {
    SshProfileConfig {
        name: "Development".to_owned(),
        host: "profile-host".to_owned(),
        user: Some("luan".to_owned()),
        port: Some(2222),
        authentication: SshAuthenticationConfig::Auto,
        host_key_policy: SshHostKeyPolicyConfig::AcceptNew,
        identity_file: None,
        proxy_jump: None,
        program: "ssh-custom".to_owned(),
        args: vec!["-v".to_owned()],
    }
}

#[test]
fn inherit_keeps_product_remote_and_clears_product_space_id() {
    let realized = realize_binding_from(
        &product(),
        &BTreeMap::new(),
        Some(MultiplexerBackendConfig::Tmux),
        &SpaceRemoteOverride::Inherit,
    );

    assert_eq!(
        realized.config.remote,
        Some(SshRemoteConfig::for_host("product-host"))
    );
    assert_eq!(realized.config.remote_space_id, None);
    assert_eq!(realized.availability_error, None);
}

#[test]
fn local_clears_product_remote_and_space_id() {
    let realized = realize_binding_from(
        &product(),
        &BTreeMap::new(),
        None,
        &SpaceRemoteOverride::Local,
    );

    assert_eq!(realized.config.remote, None);
    assert_eq!(realized.config.remote_space_id, None);
}

#[test]
fn inline_uses_exact_target() {
    let target = SshRemoteConfig {
        host: "inline-host".to_owned(),
        user: Some("remote".to_owned()),
        port: Some(2200),
        program: "ssh-wrapper".to_owned(),
        args: vec!["-o".to_owned(), "BatchMode=yes".to_owned()],
    };
    let realized = realize_binding_from(
        &product(),
        &BTreeMap::new(),
        Some(MultiplexerBackendConfig::Zellij),
        &SpaceRemoteOverride::Inline(target.clone()),
    );

    assert_eq!(realized.config.backend, MultiplexerBackendConfig::Zellij);
    assert_eq!(realized.config.remote, Some(target));
    assert_eq!(realized.config.remote_space_id, None);
    assert_eq!(realized.availability_error, None);
}

#[test]
fn profile_backend_wins_over_binding_backend_override() {
    let remote = RemoteSpaceRef {
        profile_id: "dev".to_owned(),
        remote_space_id: "space-1".to_owned(),
        remote_space_name: "Agents".to_owned(),
        backend: MultiplexerBackendConfig::Rmux,
    };
    let mut profiles = BTreeMap::new();
    profiles.insert("dev".to_owned(), profile());
    let realized = realize_binding_from(
        &product(),
        &profiles,
        Some(MultiplexerBackendConfig::Native),
        &SpaceRemoteOverride::Profile(remote),
    );

    assert_eq!(realized.config.backend, MultiplexerBackendConfig::Rmux);
    assert_eq!(realized.config.remote, Some(profile().to_remote()));
    assert_eq!(realized.config.remote_space_id.as_deref(), Some("space-1"));
}

#[test]
fn valid_profile_resolves_target_and_space() {
    let remote = RemoteSpaceRef {
        profile_id: "dev".to_owned(),
        remote_space_id: "space-1".to_owned(),
        remote_space_name: "Agents".to_owned(),
        backend: MultiplexerBackendConfig::Tmux,
    };
    let mut profiles = BTreeMap::new();
    profiles.insert("dev".to_owned(), profile());
    let realized = realize_binding_from(
        &product(),
        &profiles,
        None,
        &SpaceRemoteOverride::Profile(remote),
    );

    assert_eq!(realized.config.backend, MultiplexerBackendConfig::Tmux);
    assert_eq!(realized.config.remote, Some(profile().to_remote()));
    assert_eq!(realized.config.remote_space_id.as_deref(), Some("space-1"));
    assert_eq!(realized.availability_error, None);
}

#[test]
fn missing_profile_clears_remote_and_reports_exact_error() {
    let remote = RemoteSpaceRef {
        profile_id: "missing".to_owned(),
        remote_space_id: "space-1".to_owned(),
        remote_space_name: "Agents".to_owned(),
        backend: MultiplexerBackendConfig::Tmux,
    };
    let realized = realize_binding_from(
        &product(),
        &BTreeMap::new(),
        None,
        &SpaceRemoteOverride::Profile(remote),
    );

    assert_eq!(realized.config.remote, None);
    assert_eq!(realized.config.remote_space_id, None);
    assert_eq!(
        realized.availability_error.as_deref(),
        Some("SSH profile 'missing' is unavailable")
    );
}

#[test]
fn unsupported_remote_backend_clears_remote_placement() {
    let target = SshRemoteConfig::for_host("inline-host");
    let realized = realize_binding_from(
        &product(),
        &BTreeMap::new(),
        Some(MultiplexerBackendConfig::Native),
        &SpaceRemoteOverride::Inline(target),
    );

    assert_eq!(realized.config.backend, MultiplexerBackendConfig::Native);
    assert_eq!(realized.config.remote, None);
    assert_eq!(realized.config.remote_space_id, None);
}

#[test]
fn remote_space_id_is_cleared_before_non_profile_placement() {
    let realized = realize_binding_from(
        &product(),
        &BTreeMap::new(),
        Some(MultiplexerBackendConfig::Rmux),
        &SpaceRemoteOverride::Local,
    );

    assert_eq!(realized.config.remote_space_id, None);
}
