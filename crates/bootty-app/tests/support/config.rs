pub(super) fn config(
    config_path: std::path::PathBuf,
    backend: bootty_config::config::MultiplexerBackendConfig,
) -> bootty_config::config::BoottyConfig {
    let mut config = bootty_config::config::BoottyConfig {
        config_path,
        ..Default::default()
    };
    config.multiplexer.backend = backend;
    config
}

pub(super) fn default_config(
    config_path: std::path::PathBuf,
) -> bootty_config::config::BoottyConfig {
    bootty_config::config::BoottyConfig {
        config_path,
        ..Default::default()
    }
}
