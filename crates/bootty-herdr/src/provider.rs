use std::path::Path;

use crate::HerdrBackend;
use crate::remote::{RemoteHerdrApi, RemoteHerdrBridge};
#[cfg(feature = "app")]
use crate::{HerdrPanePolicy, herdr_capabilities};
use bootty_mux::{
    backend::MuxBackend,
    provider::{MuxBackendProvider, MuxCommandDispatch},
};
#[cfg(feature = "app")]
use bootty_mux::{
    capability::BindingCapabilityDescriptor,
    controller::SpaceId,
    provider::{
        GeneratedSessionNamePolicy, MuxAppBackendPolicy, MuxAppBackendProvider, PaneBehavior,
        PaneTopology, PersistedSessionPolicy, SelectionPublicationPolicy, TerminalProgressPolicy,
        TerminalResidency,
    },
    terminal::{
        BackendPanePolicy, PaneLayoutResizeRequest, PaneStartRequest, ScopedMuxPaneTarget,
        TerminalRuntime,
    },
};
use bootty_mux_model::{MuxBackendKind, MuxBindingConfig};

pub struct HerdrProvider;

impl MuxBackendProvider for HerdrProvider {
    fn command_dispatch(&self) -> MuxCommandDispatch {
        MuxCommandDispatch::WorkerThread
    }

    fn kind(&self) -> MuxBackendKind {
        MuxBackendKind::Herdr
    }

    fn build_backend(
        &self,
        config: &MuxBindingConfig,
        _workspace: Option<&Path>,
    ) -> Box<dyn MuxBackend> {
        let Some(target) = config.remote.clone() else {
            return Box::new(HerdrBackend::new(config.herdr_session.clone()));
        };
        match RemoteHerdrBridge::shared(target, config.herdr_session.clone()) {
            Ok(bridge) => Box::new(HerdrBackend::with_api(RemoteHerdrApi::new(bridge))),
            Err(error) => Box::new(FailedHerdrBackend(error.to_string())),
        }
    }
}

#[cfg(feature = "app")]
impl MuxAppBackendProvider for HerdrProvider {
    fn app_policy(&self) -> MuxAppBackendPolicy {
        MuxAppBackendPolicy {
            panes: PaneBehavior {
                topology: PaneTopology::Attach,
                cache_terminals: true,
                resize_cached_terminals: true,
            },
            progress: TerminalProgressPolicy::BackendSnapshot,
            persisted_sessions: PersistedSessionPolicy::AfterEmptyInitialSnapshot,
            generated_session_names: GeneratedSessionNamePolicy::PreserveBackend,
            terminal_residency: TerminalResidency::BindingScoped,
            selection_publication: SelectionPublicationPolicy::PersistBeforePublish,
        }
    }

    fn build_pane_policy(&self, config: &MuxBindingConfig) -> Box<dyn BackendPanePolicy> {
        match config.remote.clone() {
            Some(target) => {
                match HerdrPanePolicy::remote(config.herdr_session.clone(), target.clone()) {
                    Ok(policy) => Box::new(policy),
                    Err(error) => Box::new(FailedHerdrPanePolicy {
                        target,
                        error: error.to_string(),
                    }),
                }
            }
            None => Box::new(HerdrPanePolicy::new(config.herdr_session.clone())),
        }
    }

    fn capabilities(&self, scope: SpaceId) -> BindingCapabilityDescriptor {
        herdr_capabilities(scope)
    }
}

#[cfg(feature = "app")]
struct FailedHerdrPanePolicy {
    target: bootty_mux_model::SshTarget,
    error: String,
}

#[cfg(feature = "app")]
impl BackendPanePolicy for FailedHerdrPanePolicy {
    fn remote_target(&self) -> Option<&bootty_mux_model::SshTarget> {
        Some(&self.target)
    }

    fn start_terminal(
        &mut self,
        _request: PaneStartRequest<'_>,
    ) -> anyhow::Result<Option<Box<dyn TerminalRuntime>>> {
        anyhow::bail!(self.error.clone())
    }

    fn sync_target(&mut self, _target: Option<&ScopedMuxPaneTarget>, _hide_tmux_status: bool) {}

    fn set_layout_window(&mut self, _window_id: Option<&str>) {}

    fn resize_layout_window(
        &mut self,
        _request: PaneLayoutResizeRequest<'_>,
    ) -> anyhow::Result<bool> {
        anyhow::bail!(self.error.clone())
    }

    fn deactivate(&mut self) {}
}

struct FailedHerdrBackend(String);

impl MuxBackend for FailedHerdrBackend {
    fn snapshot(&self) -> anyhow::Result<bootty_mux::snapshot::MuxSnapshot> {
        anyhow::bail!(self.0.clone())
    }

    fn execute(&mut self, _command: bootty_mux::command::MuxCommand) -> anyhow::Result<()> {
        anyhow::bail!(self.0.clone())
    }
}

bootty_mux::register_mux_backend!(HerdrProvider);

pub fn link() {}
