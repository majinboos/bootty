use std::collections::BTreeMap;

use bootty_config::config::{
    BoottyConfig, MultiplexerBackendConfig, MultiplexerConfig, SshProfileConfig,
};
use bootty_workspace::SpaceRemoteOverride;

use crate::error_catalog::ErrorNotice;

/// The binding value that the app hands to the mux controller.
///
/// The product config owns the initial validated value. This module applies the placement policy
/// of one Workspace binding once. The controller and terminal pane code then consume the same
/// realized value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RealizedMuxBinding {
    pub(super) config: MultiplexerConfig,
    pub(super) availability_error: Option<String>,
    /// The Space id stamped onto sessions this binding creates.
    ///
    /// A remote binding uses the id the host on the far side knows the Space by, because that is
    /// what its daemon filters on. Everything else uses the Space's own portable id.
    pub(super) space_tag: String,
}

pub(super) fn realize_binding(
    config: &BoottyConfig,
    backend_override: Option<MultiplexerBackendConfig>,
    remote_override: &SpaceRemoteOverride,
    space_tag: String,
) -> RealizedMuxBinding {
    realize_binding_from(
        &config.multiplexer,
        &config.ssh_profiles,
        backend_override,
        remote_override,
        space_tag,
    )
}

pub(super) fn realize_binding_from(
    product: &MultiplexerConfig,
    ssh_profiles: &BTreeMap<String, SshProfileConfig>,
    backend_override: Option<MultiplexerBackendConfig>,
    remote_override: &SpaceRemoteOverride,
    space_tag: String,
) -> RealizedMuxBinding {
    let mut config = product.clone();

    config.backend = match remote_override {
        SpaceRemoteOverride::Profile(remote) => remote.backend,
        _ => backend_override.unwrap_or(config.backend),
    };

    // A product-level remote Space must never leak into a binding that did not select it.
    config.remote_space_id = None;

    let availability_error = match remote_override {
        SpaceRemoteOverride::Inherit => None,
        SpaceRemoteOverride::Local => {
            clear_remote(&mut config);
            None
        }
        SpaceRemoteOverride::Profile(remote) => {
            config.remote_space_id = Some(remote.remote_space_id.clone());
            if let Some(profile) = ssh_profiles.get(&remote.profile_id) {
                config.remote = Some(profile.to_remote());
                None
            } else {
                clear_remote(&mut config);
                Some(
                    ErrorNotice::SshProfileUnavailable(format!(
                        "SSH profile '{}' is unavailable",
                        remote.profile_id
                    ))
                    .raw_message(),
                )
            }
        }
        SpaceRemoteOverride::Inline(remote) => {
            config.remote = Some(remote.clone());
            None
        }
    };

    if !config.backend.supports_remote() {
        clear_remote(&mut config);
    }

    RealizedMuxBinding {
        space_tag: config.remote_space_id.clone().unwrap_or(space_tag),
        config,
        availability_error,
    }
}

fn clear_remote(config: &mut MultiplexerConfig) {
    config.remote = None;
    config.remote_space_id = None;
}
