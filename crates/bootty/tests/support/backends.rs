use std::sync::Arc;

use bootty_mux::provider::MuxAppBackendRegistry;

pub fn backends() -> Arc<MuxAppBackendRegistry> {
    bootty_native::link();
    bootty_rmux::link();
    bootty_tmux::link();
    bootty_zellij::link();

    Arc::new(MuxAppBackendRegistry::desktop().expect("complete executable backend registry"))
}
