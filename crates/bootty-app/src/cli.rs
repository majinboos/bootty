use std::{
    fs,
    path::{Path, PathBuf},
    process,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use bootty_config::config::{BoottyConfig, load_config_from_path};
use clap::{Parser, Subcommand, ValueEnum};

use crate::application_identity::ApplicationIdentity;

mod config_overrides;

use config_overrides::ConfigOverrides;

#[derive(Clone, Debug, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Download and install the latest Bootty release.
    Update,
    /// List commands exposed by a running Bootty instance.
    Commands,
    /// Describe one command exposed by a running Bootty instance.
    Describe { name: String },
    /// Invoke a command through the owner-local control plane.
    #[command(name = "command")]
    Invoke {
        name: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Legacy remote Space protocol retained while daemon installations roll out.
    #[command(name = "remote-space", hide = true, subcommand)]
    RemoteSpace(RemoteSpaceCommand),
    /// Legacy remote command transport retained while daemon installations roll out.
    #[command(name = "remote-exec", hide = true)]
    RemoteExec { payload: String },
    /// Legacy remote availability probe retained while daemon installations roll out.
    #[command(name = "remote-ping", hide = true)]
    RemotePing,
    /// Legacy remote terminal protocol retained while daemon installations roll out.
    #[command(name = "remote-rmux", hide = true)]
    RemoteRmux { payload: String },
    /// Invoke a command discovered from a running Bootty instance.
    #[command(external_subcommand)]
    Dynamic(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq, Subcommand)]
pub enum RemoteSpaceCommand {
    List,
    Create {
        #[arg(long)]
        name: String,
        #[arg(long, value_enum)]
        backend: RemoteSpaceBackend,
    },
    Snapshot {
        #[arg(long)]
        id: String,
        #[arg(long, value_enum)]
        backend: RemoteSpaceBackend,
    },
    Execute {
        #[arg(long)]
        id: String,
        #[arg(long, value_enum)]
        backend: RemoteSpaceBackend,
        #[arg(long)]
        payload: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum RemoteSpaceBackend {
    Rmux,
    Tmux,
    Zellij,
}

impl From<RemoteSpaceBackend> for bootty_mux::MuxBackendKind {
    fn from(value: RemoteSpaceBackend) -> Self {
        match value {
            RemoteSpaceBackend::Rmux => Self::Rmux,
            RemoteSpaceBackend::Tmux => Self::Tmux,
            RemoteSpaceBackend::Zellij => Self::Zellij,
        }
    }
}
#[derive(Debug, Parser)]
#[command(name = "bootty", version, about = "Bootty terminal emulator")]
pub struct Cli {
    /// Load config from this TOML file instead of the default XDG path.
    #[arg(long, value_name = "PATH", conflicts_with = "defaults")]
    config: Option<PathBuf>,

    /// Ignore user config and start from built-in defaults with isolated temp sidecar state.
    #[arg(long, conflicts_with = "config")]
    defaults: bool,

    /// Stable persistence identity for this application window.
    #[arg(long, default_value = "main", hide = true)]
    window_state_key: String,

    /// Print the exact JSON-RPC response.
    #[arg(long, global = true)]
    json: bool,

    /// Start a Bootty instance when none is running.
    #[arg(long, global = true)]
    start: bool,

    #[command(flatten)]
    overrides: ConfigOverrides,

    #[command(subcommand)]
    command: Option<Command>,
}

impl Cli {
    pub fn load_config(&self) -> Result<BoottyConfig> {
        let path = self.selected_config_path();
        if self.defaults {
            create_parent_dir_for_defaults(&path)?;
        }
        let mut config = load_config_from_path(&path)?;
        self.overrides.apply(&mut config)?;
        Ok(config)
    }

    pub fn window_state_key(&self) -> &str {
        &self.window_state_key
    }

    pub fn subcommand(&self) -> Option<&Command> {
        self.command.as_ref()
    }

    pub fn json(&self) -> bool {
        self.json
    }

    pub fn start(&self) -> bool {
        self.start
    }

    fn selected_config_path(&self) -> PathBuf {
        if self.defaults {
            return isolated_defaults_config_path();
        }
        self.config
            .clone()
            .unwrap_or_else(|| ApplicationIdentity::current().default_config_path())
    }
}

fn isolated_defaults_config_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir()
        .join(format!(
            "{}-defaults-{}-{nanos}",
            ApplicationIdentity::current().cli_name(),
            process::id()
        ))
        .join("config.toml")
}

fn create_parent_dir_for_defaults(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create isolated defaults directory {}",
                parent.display()
            )
        })?;
    }
    Ok(())
}
