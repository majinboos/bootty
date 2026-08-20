use std::sync::Arc;

use bootty_app::mux::provider::MuxBackendRegistry;

pub fn backends() -> Arc<MuxBackendRegistry> {
    bootty_native::link();
    bootty_rmux::link();
    bootty_tmux::link();
    bootty_zellij::link();

    Arc::new(MuxBackendRegistry::desktop().expect("complete benchmark backend registry"))
}
