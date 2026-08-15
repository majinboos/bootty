#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplicationIdentity {
    Production,
    Development,
}

impl ApplicationIdentity {
    pub const fn production() -> Self {
        Self::Production
    }

    pub const fn development() -> Self {
        Self::Development
    }

    pub const fn current() -> Self {
        if cfg!(any(debug_assertions, feature = "bootty-dev")) {
            Self::development()
        } else {
            Self::production()
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

    pub const fn endpoint_namespace(self) -> &'static str {
        self.cli_name()
    }

    pub const fn state_namespace(self) -> &'static str {
        self.cli_name()
    }

    pub const fn automatic_updates_enabled(self) -> bool {
        matches!(self, Self::Production)
    }
}
