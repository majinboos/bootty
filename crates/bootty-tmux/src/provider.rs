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

use crate::TmuxBackend;
#[cfg(feature = "app")]
use crate::{TmuxControlRunner, TmuxPanePolicy, tmux_capabilities};

pub struct TmuxProvider;

impl MuxBackendProvider for TmuxProvider {
    fn command_dispatch(&self) -> MuxCommandDispatch {
        MuxCommandDispatch::WorkerThread
    }

    fn kind(&self) -> MuxBackendKind {
        MuxBackendKind::Tmux
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
                MuxBackendKind::Tmux,
            ));
        }
        #[cfg(feature = "app")]
        {
            Box::new(match &config.remote {
                Some(remote) => TmuxBackend::with_runner(
                    "tmux",
                    TmuxControlRunner::for_remote(SshRemote::new(remote.clone())),
                ),
                None => {
                    TmuxBackend::for_identity(bootty_identity::ApplicationIdentity::for_process())
                }
            })
        }
        #[cfg(not(feature = "app"))]
        Box::new(TmuxBackend::new())
    }
}

#[cfg(feature = "app")]
impl MuxAppBackendProvider for TmuxProvider {
    fn app_policy(&self) -> MuxAppBackendPolicy {
        MuxAppBackendPolicy {
            panes: PaneBehavior {
                topology: PaneTopology::Attach,
                cache_terminals: true,
                resize_cached_terminals: true,
            },
            progress: TerminalProgressPolicy::BackendSnapshot,
            persisted_sessions: PersistedSessionPolicy::Never,
            generated_session_names: GeneratedSessionNamePolicy::Reconcile,
            terminal_residency: TerminalResidency::BindingScoped,
            selection_publication: SelectionPublicationPolicy::Direct,
        }
    }

    fn build_pane_policy(&self, config: &MuxBindingConfig) -> Box<dyn BackendPanePolicy> {
        Box::new(TmuxPanePolicy::new(
            config.remote.clone().map(SshRemote::new),
        ))
    }

    fn capabilities(&self, scope: MuxScope) -> BindingCapabilityDescriptor {
        tmux_capabilities(scope)
    }
}

bootty_mux::register_mux_backend!(TmuxProvider);

pub fn link() {}
