pub use crate::space_protocol::decode_command;
use anyhow::{Context, Result, bail};
use bootty_mux_model::MuxBackendKind;

use bootty_mux::{
    backend::MuxBackend,
    command::MuxCommand,
    process::{CommandRunner, SystemCommandRunner},
    snapshot::MuxSnapshot,
};

use crate::{
    space_protocol::encode_command,
    ssh::{REMOTE_DAEMON_PROGRAM, SshRemote, remote_daemon_failure},
};

const REMOTE_SPACE_SUBCOMMAND: &str = "remote-space";

pub struct RemoteSpaceBackend {
    remote: SshRemote,
    space_id: String,
    backend: MuxBackendKind,
}

impl RemoteSpaceBackend {
    pub fn new(remote: SshRemote, space_id: impl Into<String>, backend: MuxBackendKind) -> Self {
        Self {
            remote,
            space_id: space_id.into(),
            backend,
        }
    }

    fn run(&self, args: &[String]) -> Result<String> {
        self.remote.ensure_daemon()?;
        let (program, args) = self.remote.proxy_command(REMOTE_DAEMON_PROGRAM, args)?;
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
        let output = self.run(&[
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
        self.run(&[
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
}

fn backend_name(backend: MuxBackendKind) -> &'static str {
    match backend {
        MuxBackendKind::Herdr => "herdr",
        MuxBackendKind::Native => "native",
        MuxBackendKind::Rmux => "rmux",
        MuxBackendKind::Tmux => "tmux",
    }
}
