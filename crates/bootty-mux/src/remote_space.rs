use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bootty_config::config::MultiplexerBackendConfig;

use crate::{
    backend::MuxBackend,
    capability::BindingCapabilityDescriptor,
    command::MuxCommand,
    controller::MuxScope,
    process::{CommandRunner, SystemCommandRunner},
    rmux::rmux_capabilities,
    snapshot::MuxSnapshot,
    ssh::{SshRemote, remote_bootty_failure},
    tmux::tmux_capabilities,
    zellij::zellij_capabilities,
};

const REMOTE_SPACE_SUBCOMMAND: &str = "remote-space";
const MAX_COMMAND_PAYLOAD: usize = 1024 * 1024;

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
        let (program, args) = self.remote.proxy_command("bootty", &args)?;
        let output = SystemCommandRunner.run(&program, &args)?;
        if output.success {
            return Ok(output.stdout);
        }
        bail!(
            "{}",
            remote_bootty_failure(self.remote.host(), &output.stderr)
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

fn encode_command(command: &MuxCommand) -> Result<String> {
    let bytes = serde_json::to_vec(command).context("encode remote Space command")?;
    if bytes.len() > MAX_COMMAND_PAYLOAD {
        bail!("remote Space command is too large")
    }
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub fn decode_command(payload: &str) -> Result<MuxCommand> {
    if payload.len() > MAX_COMMAND_PAYLOAD * 2 {
        bail!("remote Space command is too large")
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .context("decode remote Space command")?;
    serde_json::from_slice(&bytes).context("parse remote Space command")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_payload_preserves_arguments() {
        let command = MuxCommand::RenameSession {
            session_id: "space ; $HOME".to_owned(),
            name: "work & play".to_owned(),
        };

        assert_eq!(
            decode_command(&encode_command(&command).unwrap()).unwrap(),
            command
        );
    }
}
