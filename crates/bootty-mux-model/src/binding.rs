use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};
use strum::{Display, EnumIter};

/// A backend that Bootty can drive for one terminal binding.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Display, EnumIter, Hash, Serialize, PartialEq, Eq,
)]
#[serde(rename_all = "kebab-case")]
pub enum MuxBackendKind {
    Rmux,
    #[default]
    Native,
    Tmux,
    Zellij,
}

/// The versioned wire value returned by the remote Space catalog.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RemoteSpaceSummary {
    pub catalog_version: u32,
    pub id: String,
    pub name: String,
    pub backend: MuxBackendKind,
}

impl MuxBackendKind {
    /// Returns whether the backend has a client that can run on another host.
    pub const fn supports_remote(self) -> bool {
        matches!(self, Self::Rmux | Self::Tmux | Self::Zellij)
    }
}

/// One resolved SSH process target.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SshTarget {
    /// SSH config alias, hostname, or address of the host running the multiplexer.
    pub host: String,
    /// Login user, when it is neither the local user nor covered by `~/.ssh/config`.
    #[serde(default)]
    pub user: Option<String>,
    /// SSH port, when it is neither 22 nor covered by `~/.ssh/config`.
    #[serde(default)]
    pub port: Option<u16>,
    /// The SSH client to run.
    #[serde(default = "default_ssh_program")]
    pub program: String,
    /// Extra flags handed to the SSH client before the destination.
    #[serde(default)]
    pub args: Vec<String>,
}

fn default_ssh_program() -> String {
    "ssh".to_owned()
}

impl SshTarget {
    /// Create a target that relies on the host's SSH configuration and defaults.
    pub fn for_host(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            user: None,
            port: None,
            program: default_ssh_program(),
            args: Vec::new(),
        }
    }
}

/// The operational configuration for one multiplexer binding.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MuxBindingConfig {
    pub backend: MuxBackendKind,
    /// Hide tmux's own status bar in Bootty's client.
    pub hide_tmux_status: bool,
    /// Reach the multiplexer on another host over SSH.
    pub remote: Option<SshTarget>,
    /// The remote-owned Space selected through a named SSH profile.
    pub remote_space_id: Option<String>,
}

/// Validation failure for an operational multiplexer binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MuxBindingConfigError {
    EmptyRemoteHost,
    UnsupportedRemoteBackend { backend: MuxBackendKind },
}

impl fmt::Display for MuxBindingConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRemoteHost => {
                formatter.write_str("multiplexer.remote.host must name a host")
            }
            Self::UnsupportedRemoteBackend { backend } => write!(
                formatter,
                "multiplexer.remote needs a backend with a client to run there, got {backend:?}"
            ),
        }
    }
}

impl Error for MuxBindingConfigError {}

impl MuxBindingConfig {
    /// Validate remote placement without changing the binding.
    pub fn validate_remote(&self) -> Result<(), MuxBindingConfigError> {
        let Some(remote) = &self.remote else {
            return Ok(());
        };
        if remote.host.trim().is_empty() {
            return Err(MuxBindingConfigError::EmptyRemoteHost);
        }
        if self.backend.supports_remote() {
            return Ok(());
        }
        Err(MuxBindingConfigError::UnsupportedRemoteBackend {
            backend: self.backend,
        })
    }
}
