use std::path::PathBuf;

use bootty_identity::ApplicationIdentity;

pub fn prepare_local_rmux_daemon(identity: ApplicationIdentity) -> anyhow::Result<()> {
    crate::rmux_bridge::prepare_local_rmux_daemon(identity)
}

pub(crate) fn endpoint_path() -> anyhow::Result<PathBuf> {
    endpoint_path_for(ApplicationIdentity::for_process())
}

pub fn endpoint_path_for(identity: ApplicationIdentity) -> anyhow::Result<PathBuf> {
    let mut endpoint = rmux_ipc::default_endpoint()?.into_path();
    endpoint.set_file_name(socket_name(identity, rmux_proto::RMUX_WIRE_VERSION));
    Ok(endpoint)
}

/// Two builds can share one endpoint exactly when their wire versions match.
pub fn socket_name(identity: ApplicationIdentity, wire_version: u32) -> String {
    match identity {
        ApplicationIdentity::Production => format!("bootty-wire{wire_version}"),
        ApplicationIdentity::Development => format!("bootty-dev-wire{wire_version}"),
    }
}
