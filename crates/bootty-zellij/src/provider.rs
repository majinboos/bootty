use std::path::Path;

use bootty_mux::{
    backend::MuxBackend,
    provider::{MuxBackendProvider, MuxCommandDispatch},
};
#[cfg(feature = "app")]
use bootty_mux::{
    capability::BindingCapabilityDescriptor,
    controller::MuxScope,
    provider::{
        GeneratedSessionNamePolicy, MuxAppBackendPolicy, MuxAppBackendProvider, PaneBehavior,
        PaneTopology, PersistedSessionPolicy, SelectionPublicationPolicy, TerminalProgressPolicy,
        TerminalResidency,
    },
    terminal::BackendPanePolicy,
};
use bootty_mux_model::{MuxBackendKind, MuxBindingConfig};
use bootty_remote::{space::RemoteSpaceBackend, ssh::SshRemote};

use crate::ZellijBackend;
#[cfg(feature = "app")]
use crate::{ZellijPanePolicy, zellij_capabilities};

pub struct ZellijProvider;

impl MuxBackendProvider for ZellijProvider {
    fn command_dispatch(&self) -> MuxCommandDispatch {
        MuxCommandDispatch::WorkerThread
    }

    fn kind(&self) -> MuxBackendKind {
        MuxBackendKind::Zellij
    }

    fn build_backend(
        &self,
        config: &MuxBindingConfig,
        _workspace: Option<&Path>,
    ) -> Box<dyn MuxBackend> {
        if let (Some(remote), Some(space_id)) = (&config.remote, &config.remote_space_id) {
            return Box::new(RemoteSpaceBackend::new(
                SshRemote::new(remote.clone()),
                space_id.clone(),
                MuxBackendKind::Zellij,
            ));
        }
        match &config.remote {
            Some(remote) => Box::new(ZellijBackend::with_runner(
                bootty_remote::ssh::SshCommandRunner::new(
                    SshRemote::new(remote.clone()),
                    bootty_mux::process::SystemCommandRunner,
                ),
            )),
            None => match ZellijBackend::for_identity(
                bootty_identity::ApplicationIdentity::for_process(),
            ) {
                Ok(backend) => Box::new(backend),
                Err(error) => Box::new(FailedBackend(error.to_string())),
            },
        }
    }
}

#[cfg(feature = "app")]
impl MuxAppBackendProvider for ZellijProvider {
    fn app_policy(&self) -> MuxAppBackendPolicy {
        MuxAppBackendPolicy {
            panes: PaneBehavior {
                topology: PaneTopology::Attach,
                cache_terminals: false,
                resize_cached_terminals: false,
            },
            progress: TerminalProgressPolicy::TerminalOsc,
            persisted_sessions: PersistedSessionPolicy::Never,
            generated_session_names: GeneratedSessionNamePolicy::Reconcile,
            terminal_residency: TerminalResidency::BindingScoped,
            selection_publication: SelectionPublicationPolicy::Direct,
        }
    }

    fn build_pane_policy(&self, config: &MuxBindingConfig) -> Box<dyn BackendPanePolicy> {
        Box::new(ZellijPanePolicy::new(
            config.remote.clone().map(SshRemote::new),
        ))
    }

    fn capabilities(&self, scope: MuxScope) -> BindingCapabilityDescriptor {
        zellij_capabilities(scope)
    }
}

struct FailedBackend(String);

impl MuxBackend for FailedBackend {
    fn snapshot(&self) -> anyhow::Result<bootty_mux::snapshot::MuxSnapshot> {
        anyhow::bail!(self.0.clone())
    }

    fn execute(&mut self, _command: bootty_mux::command::MuxCommand) -> anyhow::Result<()> {
        anyhow::bail!(self.0.clone())
    }
}

bootty_mux::register_mux_backend!(ZellijProvider);

pub fn link() {}
