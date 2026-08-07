use std::sync::Arc;

pub type RepaintHandle = Arc<dyn Fn() + Send + Sync + 'static>;
pub use rmux_bridge::run_embedded_rmux_daemon;

const BOOTTY_RMUX_ABI_VERSION: &str = "0.9.1";

fn bootty_rmux_endpoint_path() -> anyhow::Result<std::path::PathBuf> {
    let mut endpoint = rmux_ipc::default_endpoint()?.into_path();
    endpoint.set_file_name(format!("bootty-{BOOTTY_RMUX_ABI_VERSION}"));
    Ok(endpoint)
}

pub mod backend;
pub mod capability;
pub mod command;
pub mod config;
pub mod controller;
pub mod native;
pub mod process;
pub mod rmux;
pub(crate) mod rmux_bridge;
pub mod snapshot;
pub mod terminal;
pub mod tmux;
pub mod tmux_control;
pub mod tmux_protocol;
pub mod zellij;

#[cfg(test)]
mod tests {
    #[test]
    fn rmux_endpoint_is_versioned_by_embedded_abi() {
        let endpoint = super::bootty_rmux_endpoint_path().expect("resolve rmux endpoint");

        assert_eq!(
            endpoint.file_name().and_then(|name| name.to_str()),
            Some("bootty-0.9.1")
        );
    }
}
