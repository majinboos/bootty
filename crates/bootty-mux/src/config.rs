use bootty_config::config::{MultiplexerBackendConfig, MultiplexerConfig};

use super::{
    backend::MuxBackend, native::NativeBackend, rmux::RmuxBackend, tmux::TmuxBackend,
    zellij::ZellijBackend,
};

pub fn selected_backend(config: &MultiplexerConfig) -> MultiplexerBackendConfig {
    if cfg!(windows) && config.backend == MultiplexerBackendConfig::Tmux {
        return MultiplexerBackendConfig::Native;
    }
    config.backend
}

pub fn build_backend(config: &MultiplexerConfig) -> Box<dyn MuxBackend> {
    match selected_backend(config) {
        MultiplexerBackendConfig::Rmux => Box::new(RmuxBackend::new()),
        MultiplexerBackendConfig::Native => Box::new(NativeBackend::new()),
        MultiplexerBackendConfig::Tmux => Box::new(TmuxBackend::new()),
        MultiplexerBackendConfig::Zellij => Box::new(ZellijBackend::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bootty_config::config::MultiplexerConfig;

    #[test]
    fn selected_backend_resolves_configured_backend() {
        for (backend, expected) in [
            (
                MultiplexerBackendConfig::Rmux,
                MultiplexerBackendConfig::Rmux,
            ),
            (
                MultiplexerBackendConfig::Native,
                MultiplexerBackendConfig::Native,
            ),
            (
                MultiplexerBackendConfig::Tmux,
                if cfg!(windows) {
                    MultiplexerBackendConfig::Native
                } else {
                    MultiplexerBackendConfig::Tmux
                },
            ),
            (
                MultiplexerBackendConfig::Zellij,
                MultiplexerBackendConfig::Zellij,
            ),
        ] {
            let config = MultiplexerConfig {
                backend,
                ..Default::default()
            };

            assert_eq!(selected_backend(&config), expected);
        }
    }
}
