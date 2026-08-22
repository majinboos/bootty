use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplicationIdentity {
    Production,
    Development,
}

impl ApplicationIdentity {
    pub const fn current() -> Self {
        if cfg!(any(debug_assertions, feature = "bootty-dev")) {
            Self::Development
        } else {
            Self::Production
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Production => "Bootty",
            Self::Development => "BoottyDev",
        }
    }

    pub const fn cli_name(self) -> &'static str {
        match self {
            Self::Production => "bootty",
            Self::Development => "bootty-dev",
        }
    }

    pub fn default_config_path(self) -> PathBuf {
        let production_path = bootty_config::config::default_config_path();
        match self {
            Self::Production => production_path,
            Self::Development => {
                let config_directory = production_path
                    .parent()
                    .map_or_else(|| PathBuf::from("bootty"), PathBuf::from);
                config_directory
                    .with_file_name(self.cli_name())
                    .join("config.toml")
            }
        }
    }

    pub const fn automatic_updates_enabled(self) -> bool {
        matches!(self, Self::Production)
    }
}
