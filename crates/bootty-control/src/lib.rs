mod catalog;
mod client;
mod lease;
mod plane;
mod protocol;
mod server;
mod state;

pub use catalog::ControlCatalog;
pub use client::{invoke_instance, invoke_or_start, running_instance, select_or_start};
pub use lease::InstanceDescriptor;
pub use plane::ControlPlane;
pub use protocol::{PROTOCOL_VERSION, RpcError, RpcRequest, RpcResponse};
pub use server::ControlServer;
