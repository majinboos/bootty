pub use crate::remote_space_protocol::decode_command;
use anyhow::{Context, Result};
use bootty_config::config::MultiplexerBackendConfig;

use crate::{
    backend::{MuxBackend, MuxBackendOperationError},
    capability::{BindingCapabilityDescriptor, BindingOperationOutcome},
    command::{MuxCommand, MuxSessionLaunchPlan},
    controller::MuxScope,
    operation::MuxBackendCommandCompletion,
    process::{CommandRunner, SystemCommandRunner},
    remote_operation_protocol::{decode_remote_operation_completion, remote_operation_failure},
    remote_space_protocol::encode_command,
    rmux::rmux_capabilities,
    rmux_bridge::supports_rmux_session_launch_plan,
    snapshot::MuxSnapshot,
    ssh::{REMOTE_DAEMON_PROGRAM, SshRemote},
    tmux::{supports_tmux_session_launch_plan, tmux_capabilities},
    zellij::zellij_capabilities,
};

const REMOTE_SPACE_SUBCOMMAND: &str = "remote-space";

pub struct RemoteSpaceBackend {
    remote: SshRemote,
    space_id: String,
    backend: MultiplexerBackendConfig,
    completion: Option<MuxBackendCommandCompletion>,
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
            completion: None,
        }
    }

    fn run(&self, args: Vec<String>) -> Result<String> {
        self.remote.ensure_daemon()?;
        let (program, args) = self.remote.proxy_command(REMOTE_DAEMON_PROGRAM, &args)?;
        let output = SystemCommandRunner.run(&program, &args)?;
        if output.success {
            return Ok(output.stdout);
        }
        Err(remote_operation_failure(self.remote.host(), &output.stderr))
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
        self.completion = None;
        if let MuxCommand::CreateSession { plan } = &command {
            plan.validate()?;
            if !supports_remote_session_launch(self.backend, plan) {
                return Err(MuxBackendOperationError::unsupported(
                    "remote backend cannot preserve this recursive session launch plan",
                )
                .into());
            }
        }
        let output = self.run(vec![
            REMOTE_SPACE_SUBCOMMAND.to_owned(),
            "execute".to_owned(),
            "--id".to_owned(),
            self.space_id.clone(),
            "--backend".to_owned(),
            backend_name(self.backend).to_owned(),
            "--payload".to_owned(),
            encode_command(&command)?,
        ])?;
        self.completion = decode_remote_operation_completion(&output)
            .context("decode remote Space command completion")?;
        Ok(())
    }

    fn execute_session_launch(
        &mut self,
        plan: MuxSessionLaunchPlan,
    ) -> BindingOperationOutcome<Result<()>> {
        self.completion = None;
        if plan.validate().is_err() || !supports_remote_session_launch(self.backend, &plan) {
            return BindingOperationOutcome::Unsupported;
        }
        BindingOperationOutcome::Supported(self.execute(MuxCommand::CreateSession { plan }))
    }

    fn session_launch_capability(
        &self,
        plan: &MuxSessionLaunchPlan,
    ) -> BindingOperationOutcome<()> {
        (plan.validate().is_ok() && supports_remote_session_launch(self.backend, plan))
            .then_some(())
            .map_or(
                BindingOperationOutcome::Unsupported,
                BindingOperationOutcome::Supported,
            )
    }

    fn take_authoritative_completion(&mut self) -> Option<MuxBackendCommandCompletion> {
        self.completion.take()
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

fn supports_remote_session_launch(
    backend: MultiplexerBackendConfig,
    plan: &MuxSessionLaunchPlan,
) -> bool {
    match backend {
        MultiplexerBackendConfig::Rmux => supports_rmux_session_launch_plan(plan),
        MultiplexerBackendConfig::Tmux => supports_tmux_session_launch_plan(plan),
        MultiplexerBackendConfig::Native | MultiplexerBackendConfig::Zellij => false,
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
