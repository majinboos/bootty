mod backend;
mod control;
mod model;
#[cfg(feature = "app")]
mod pane;
mod provider;
mod remote;

#[cfg(feature = "app")]
pub use backend::herdr_capabilities;
pub use backend::{HerdrBackend, project_snapshot};
pub use control::{CliHerdrApi, HerdrApi};
pub use model::{
    HerdrLayout, HerdrLayoutPane, HerdrLayoutSplit, HerdrPane, HerdrRect, HerdrSessionSnapshot,
    HerdrTab, HerdrWorkspace,
};
#[cfg(feature = "app")]
pub use pane::HerdrPanePolicy;
pub use provider::link;
pub use remote::{
    RemoteHerdrBridgePlan, RemoteHerdrStatus, parse_remote_status, remote_status_command,
};
