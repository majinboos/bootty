use std::path::Path;

use crate::HerdrBackend;
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
    terminal::BackendPanePolicy,
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
        Box::new(HerdrBackend::new(config.herdr_session.clone()))
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
        Box::new(HerdrPanePolicy::new(config.herdr_session.clone()))
    }

    fn capabilities(&self, scope: SpaceId) -> BindingCapabilityDescriptor {
        herdr_capabilities(scope)
    }
}

bootty_mux::register_mux_backend!(HerdrProvider);

pub fn link() {}
