mod backend;
mod bridge;
mod local;
#[cfg(feature = "app")]
mod pane;
mod pane_io;
mod provider;
mod remote;

#[cfg(feature = "app")]
pub use backend::rmux_capabilities;
pub use backend::{RmuxBackend, RmuxControl};
pub use bridge::{prepare_local_rmux_daemon, run_embedded_rmux_daemon};
pub use local::{endpoint_path_for, socket_name};
#[cfg(feature = "app")]
pub use pane::RmuxPanePolicy;
pub use provider::link;
pub use remote::{RemoteRmuxRequest, run_remote_rmux_command};
pub use rmux_client::INTERNAL_DAEMON_FLAG;
