use anyhow::Result;

fn main() -> Result<()> {
    let config =
        bootty_app::config::load_config_from_path(bootty_app::config::default_config_path())?;
    let options = bootty_app::platform::native_options_for_config(&config);

    bootty_app::native_host::run(options, config)
}
