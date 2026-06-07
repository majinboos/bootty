use crate::config::{MultiplexerBackendConfig, MultiplexerConfig};

use super::{
    backend::MuxBackend, native::NativeBackend, rmux::RmuxBackend, tmux::TmuxBackend,
    zellij::ZellijBackend,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MuxBackendKind {
    Rmux,
    Native,
    Tmux,
    Zellij,
}

impl From<MultiplexerBackendConfig> for MuxBackendKind {
    fn from(value: MultiplexerBackendConfig) -> Self {
        match value {
            MultiplexerBackendConfig::Rmux => Self::Rmux,
            MultiplexerBackendConfig::Native => Self::Native,
            MultiplexerBackendConfig::Tmux => Self::Tmux,
            MultiplexerBackendConfig::Zellij => Self::Zellij,
        }
    }
}

pub fn selected_backend(config: &MultiplexerConfig) -> MuxBackendKind {
    config.backend.into()
}

pub fn build_backend(config: &MultiplexerConfig) -> Box<dyn MuxBackend> {
    match selected_backend(config) {
        MuxBackendKind::Rmux => Box::new(RmuxBackend::new()),
        MuxBackendKind::Native => Box::new(NativeBackend::new()),
        MuxBackendKind::Tmux => Box::new(TmuxBackend::new()),
        MuxBackendKind::Zellij => Box::new(ZellijBackend::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MultiplexerConfig;

    #[test]
    fn selected_backend_preserves_configured_backend_without_fallback() {
        for (backend, expected) in [
            (MultiplexerBackendConfig::Rmux, MuxBackendKind::Rmux),
            (MultiplexerBackendConfig::Native, MuxBackendKind::Native),
            (MultiplexerBackendConfig::Tmux, MuxBackendKind::Tmux),
            (MultiplexerBackendConfig::Zellij, MuxBackendKind::Zellij),
        ] {
            let config = MultiplexerConfig { backend };

            assert_eq!(selected_backend(&config), expected);
        }
    }

    #[test]
    fn backend_factory_instantiates_configured_backend_without_fallback() {
        for (backend, expected) in [
            (MultiplexerBackendConfig::Rmux, MuxBackendKind::Rmux),
            (MultiplexerBackendConfig::Native, MuxBackendKind::Native),
            (MultiplexerBackendConfig::Tmux, MuxBackendKind::Tmux),
            (MultiplexerBackendConfig::Zellij, MuxBackendKind::Zellij),
        ] {
            let config = MultiplexerConfig { backend };

            assert_eq!(build_backend(&config).kind(), expected);
        }
    }
}
