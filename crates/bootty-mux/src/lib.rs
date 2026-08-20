#[cfg(feature = "app")]
use std::sync::Arc;

#[cfg(feature = "app")]
pub type RepaintHandle = Arc<dyn Fn() + Send + Sync + 'static>;
pub use bootty_mux_model::{MuxBackendKind, MuxBindingConfig, RemoteSpaceSummary, SshTarget};
pub use remote_exec::{REMOTE_DAEMON_PROGRAM, REMOTE_DAEMON_PROTOCOL_VERSION, run_remote_command};
pub use remote_space_protocol::{
    decode_command as decode_remote_space_command, encode_command as encode_remote_space_command,
};
pub use rmux_bridge::{run_embedded_rmux_daemon, start_embedded_rmux_daemon_for_tests};
pub use rmux_client::INTERNAL_DAEMON_FLAG as INTERNAL_RMUX_DAEMON_FLAG;
pub use rmux_remote::run_remote_rmux_command;

pub fn prepare_local_rmux_daemon(
    identity: bootty_identity::ApplicationIdentity,
) -> anyhow::Result<()> {
    rmux_bridge::prepare_local_rmux_daemon(identity)
}

/// Bootty's own rmux daemon, named for the wire protocol it speaks rather than the release it was
/// built from.
///
/// A client and a daemon understand each other exactly when their wire versions match — rmux's
/// `SUPPORTED_WIRE_VERSION` is that single version and nothing else. Naming the socket after the
/// crate release made those two things disagree in both directions: builds that differ only in
/// release number stopped sharing a daemon, and an upgrade that did change the protocol met the old
/// daemon still listening on the same path, which answers every request with "running daemon uses
/// an incompatible protocol". Deriving the name from the protocol keeps the socket and what can be
/// spoken on it in step, with no constant to remember to bump.
fn bootty_rmux_endpoint_path() -> anyhow::Result<std::path::PathBuf> {
    bootty_rmux_endpoint_path_for(bootty_identity::ApplicationIdentity::for_process())
}

pub fn bootty_rmux_endpoint_path_for(
    identity: bootty_identity::ApplicationIdentity,
) -> anyhow::Result<std::path::PathBuf> {
    let mut endpoint = rmux_ipc::default_endpoint()?.into_path();
    endpoint.set_file_name(bootty_rmux_socket_name(
        identity,
        rmux_proto::RMUX_WIRE_VERSION,
    ));
    Ok(endpoint)
}

pub fn bootty_rmux_socket_name(
    identity: bootty_identity::ApplicationIdentity,
    wire_version: u32,
) -> String {
    match identity {
        bootty_identity::ApplicationIdentity::Production => format!("bootty-wire{wire_version}"),
        bootty_identity::ApplicationIdentity::Development => {
            format!("bootty-dev-wire{wire_version}")
        }
    }
}

#[cfg(feature = "app")]
pub mod backend;
#[cfg(feature = "app")]
pub mod capability;
pub mod command;
#[cfg(feature = "app")]
pub mod config;
#[cfg(feature = "app")]
pub mod controller;
pub mod membership;
#[cfg(feature = "app")]
pub mod native;
pub mod process;
pub mod project;
mod remote_exec;
#[cfg(feature = "remote-install")]
mod remote_install;
#[cfg(feature = "app")]
pub mod remote_space;
pub mod remote_space_protocol;
pub mod rmux;
pub(crate) mod rmux_bridge;
pub(crate) mod rmux_remote;
pub mod snapshot;
#[cfg(feature = "app")]
pub mod ssh;
#[cfg(feature = "app")]
pub mod terminal;
pub mod tmux;
#[cfg(feature = "app")]
pub mod tmux_control;
pub mod tmux_protocol;
pub mod zellij;

#[cfg(test)]
mod tests {
    use super::{bootty_rmux_endpoint_path, bootty_rmux_socket_name};
    use bootty_identity::ApplicationIdentity;

    /// Two builds meet on this path and then have to speak to each other, so the name has to carry
    /// the one thing that decides whether they can: the wire version. A name that changes for any
    /// other reason splits daemons that were compatible, and a name that stays put across a
    /// protocol change hands a client a daemon it cannot talk to.
    #[test]
    fn the_rmux_socket_name_tracks_the_wire_protocol_and_nothing_else() {
        assert_ne!(
            bootty_rmux_socket_name(ApplicationIdentity::Production, 8),
            bootty_rmux_socket_name(ApplicationIdentity::Production, 9)
        );
        assert_eq!(
            bootty_rmux_socket_name(ApplicationIdentity::Production, 8),
            "bootty-wire8"
        );
        assert_eq!(
            bootty_rmux_socket_name(ApplicationIdentity::Development, 8),
            "bootty-dev-wire8"
        );

        let endpoint = bootty_rmux_endpoint_path().expect("resolve rmux endpoint");
        assert_eq!(
            endpoint.file_name().and_then(|name| name.to_str()),
            Some(
                bootty_rmux_socket_name(
                    ApplicationIdentity::for_process(),
                    rmux_proto::RMUX_WIRE_VERSION,
                )
                .as_str()
            )
        );
    }
}
