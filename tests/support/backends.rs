use std::sync::Arc;

use bootty_mux::{MuxBackendKind, provider::MuxAppBackendRegistry};

pub fn backends() -> Arc<MuxAppBackendRegistry> {
    bootty_rmux::link();

    Arc::new(
        MuxAppBackendRegistry::collect([MuxBackendKind::Rmux])
            .expect("complete config writeback test backend registry"),
    )
}
