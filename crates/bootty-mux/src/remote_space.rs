pub use crate::remote_space_protocol::decode_command;
use anyhow::{Context, Result, bail};
use bootty_config::config::MultiplexerBackendConfig;

use crate::{
    backend::MuxBackend,
    capability::BindingCapabilityDescriptor,
    command::MuxCommand,
    controller::MuxScope,
    process::{CommandRunner, SystemCommandRunner},
    remote_space_protocol::encode_command,
    rmux::rmux_capabilities,
    snapshot::MuxSnapshot,
    ssh::{REMOTE_DAEMON_PROGRAM, SshRemote, remote_daemon_failure},
    tmux::tmux_capabilities,
    zellij::zellij_capabilities,
};

const REMOTE_SPACE_SUBCOMMAND: &str = "remote-space";

pub struct RemoteSpaceBackend {
    remote: SshRemote,
    space_id: String,
    backend: MultiplexerBackendConfig,
}

impl RemoteSpaceBackend {
    pub fn new(
        remote: SshRemote,
        space_id: impl Into<String>,
        backend: MultiplexerBackendConfig,
    ) -> Self {
        Self {
            remote,
            space_id: space_id.into(),
            backend,
        }
    }

    fn run(&self, args: Vec<String>) -> Result<String> {
        self.remote.ensure_daemon()?;
        let (program, args) = self.remote.proxy_command(REMOTE_DAEMON_PROGRAM, &args)?;
        let output = SystemCommandRunner.run(&program, &args)?;
        if output.success {
            return Ok(output.stdout);
        }
        bail!(
            "{}",
            remote_daemon_failure(self.remote.host(), &output.stderr)
        )
    }
}

impl MuxBackend for RemoteSpaceBackend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        let output = self.run(vec![
            REMOTE_SPACE_SUBCOMMAND.to_owned(),
            "snapshot".to_owned(),
            "--id".to_owned(),
            self.space_id.clone(),
            "--backend".to_owned(),
            backend_name(self.backend).to_owned(),
        ])?;
        serde_json::from_str(&output).context("decode remote Space snapshot")
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        self.run(vec![
            REMOTE_SPACE_SUBCOMMAND.to_owned(),
            "execute".to_owned(),
            "--id".to_owned(),
            self.space_id.clone(),
            "--backend".to_owned(),
            backend_name(self.backend).to_owned(),
            "--payload".to_owned(),
            encode_command(&command)?,
        ])?;
        Ok(())
    }

    fn capabilities(&self, scope: MuxScope) -> BindingCapabilityDescriptor {
        match self.backend {
            MultiplexerBackendConfig::Rmux => rmux_capabilities(scope),
            MultiplexerBackendConfig::Tmux => tmux_capabilities(scope),
            MultiplexerBackendConfig::Zellij => zellij_capabilities(scope),
            MultiplexerBackendConfig::Native => BindingCapabilityDescriptor::new(scope, []),
        }
    }
}

fn backend_name(backend: MultiplexerBackendConfig) -> &'static str {
    match backend {
        MultiplexerBackendConfig::Native => "native",
        MultiplexerBackendConfig::Rmux => "rmux",
        MultiplexerBackendConfig::Tmux => "tmux",
        MultiplexerBackendConfig::Zellij => "zellij",
    }
}
