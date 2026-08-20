use std::{
    error::Error,
    fmt,
    path::{Path, PathBuf},
    sync::OnceLock,
};

static PROCESS_IDENTITY: OnceLock<ApplicationIdentity> = OnceLock::new();

pub const APPLICATION_IDENTITY_ENV: &str = "BOOTTY_APPLICATION_IDENTITY";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplicationIdentity {
    Production,
    Development,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApplicationIdentityConflict {
    active: ApplicationIdentity,
    requested: ApplicationIdentity,
}

impl fmt::Display for ApplicationIdentityConflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "application identity is already initialized as {:?}; cannot change it to {:?}",
            self.active, self.requested
        )
    }
}

impl Error for ApplicationIdentityConflict {}

impl ApplicationIdentity {
    pub const fn current() -> Self {
        if cfg!(any(debug_assertions, feature = "bootty-dev")) {
            Self::Development
        } else {
            Self::Production
        }
    }

    pub fn for_process() -> Self {
        PROCESS_IDENTITY.get().copied().unwrap_or(Self::Production)
    }

    pub fn initialize_process(self) -> Result<(), ApplicationIdentityConflict> {
        if PROCESS_IDENTITY.set(self).is_err() && Self::for_process() != self {
            return Err(ApplicationIdentityConflict {
                active: Self::for_process(),
                requested: self,
            });
        }
        Ok(())
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Production => "Bootty",
            Self::Development => "BoottyDev",
        }
    }

    pub const fn namespace(self) -> &'static str {
        match self {
            Self::Production => "bootty",
            Self::Development => "bootty-dev",
        }
    }

    pub const fn cli_name(self) -> &'static str {
        self.namespace()
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "bootty" => Some(Self::Production),
            "bootty-dev" => Some(Self::Development),
            _ => None,
        }
    }

    pub fn default_config_path(self) -> PathBuf {
        config_path_from_env(
            self,
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )
    }

    pub const fn automatic_updates_enabled(self) -> bool {
        matches!(self, Self::Production)
    }
}

pub fn config_path_from_env(
    identity: ApplicationIdentity,
    xdg_config_home: Option<impl AsRef<Path>>,
    home: Option<impl AsRef<Path>>,
) -> PathBuf {
    if let Some(xdg) = xdg_config_home {
        return xdg.as_ref().join(identity.namespace()).join("config.toml");
    }
    if let Some(home) = home {
        return home
            .as_ref()
            .join(".config")
            .join(identity.namespace())
            .join("config.toml");
    }
    PathBuf::from(identity.namespace()).join("config.toml")
}

pub fn legacy_config_path_from_env(
    identity: ApplicationIdentity,
    xdg_config_home: Option<&Path>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    xdg_config_home
        .map(Path::to_path_buf)
        .or_else(|| home.map(|home| home.join(".config")))
        .map(|root| root.join(identity.namespace()).join("config.toml"))
}

pub fn unix_daemon_state_path(
    identity: ApplicationIdentity,
    explicit: Option<&Path>,
    xdg_state_home: Option<&Path>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(explicit) = explicit {
        return Some(explicit.to_path_buf());
    }
    xdg_state_home
        .map(Path::to_path_buf)
        .or_else(|| home.map(|home| home.join(".local/state")))
        .map(|root| root.join(identity.namespace()).join("daemon.sqlite"))
}

pub fn windows_daemon_state_path(
    identity: ApplicationIdentity,
    explicit: Option<&Path>,
    local_app_data: Option<&Path>,
    app_data: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(explicit) = explicit {
        return Some(explicit.to_path_buf());
    }
    local_app_data
        .or(app_data)
        .map(|root| root.join(identity.namespace()).join("daemon.sqlite"))
}
