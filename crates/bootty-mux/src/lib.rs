#[cfg(feature = "app")]
use std::sync::Arc;

#[cfg(feature = "app")]
pub type RepaintHandle = Arc<dyn Fn() + Send + Sync + 'static>;
pub use bootty_mux_model::{MuxBackendKind, MuxBindingConfig, RemoteSpaceSummary, SshTarget};
pub use local_rmux::{
    endpoint_path_for as bootty_rmux_endpoint_path_for, prepare_local_rmux_daemon,
    socket_name as bootty_rmux_socket_name,
};
pub use remote_exec::{REMOTE_DAEMON_PROGRAM, REMOTE_DAEMON_PROTOCOL_VERSION, run_remote_command};
pub use remote_space_protocol::{
    decode_command as decode_remote_space_command, encode_command as encode_remote_space_command,
};
pub use rmux_bridge::{run_embedded_rmux_daemon, start_embedded_rmux_daemon_for_tests};
pub use rmux_client::INTERNAL_DAEMON_FLAG as INTERNAL_RMUX_DAEMON_FLAG;
pub use rmux_remote::run_remote_rmux_command;

#[cfg(feature = "app")]
pub mod backend;
#[cfg(feature = "app")]
pub mod capability;
pub mod command;
#[cfg(feature = "app")]
pub mod config;
#[cfg(feature = "app")]
pub mod controller;
mod local_rmux;
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
