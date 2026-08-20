mod backend;
mod provider;

pub use backend::ZellijBackend;
#[cfg(feature = "app")]
pub use backend::{ZellijPanePolicy, zellij_capabilities};
pub use provider::link;
