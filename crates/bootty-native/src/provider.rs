use std::path::Path;

use bootty_mux::{
    backend::MuxBackend,
    capability::BindingCapabilityDescriptor,
    controller::MuxScope,
    provider::{
        GeneratedSessionNamePolicy, MuxAppBackendPolicy, MuxAppBackendProvider, MuxBackendProvider,
        MuxCommandDispatch, PaneBehavior, PaneTopology, PersistedSessionPolicy,
        SelectionPublicationPolicy, TerminalProgressPolicy, TerminalResidency,
    },
    terminal::BackendPanePolicy,
};
use bootty_mux_model::{MuxBackendKind, MuxBindingConfig};

use crate::{NativeBackend, NativePanePolicy, native_capabilities};

pub struct NativeProvider;

impl MuxBackendProvider for NativeProvider {
    fn kind(&self) -> MuxBackendKind {
        MuxBackendKind::Native
    }

    fn command_dispatch(&self) -> MuxCommandDispatch {
        MuxCommandDispatch::CallerThread
    }

    fn build_backend(
        &self,
        _config: &MuxBindingConfig,
        workspace: Option<&Path>,
    ) -> Box<dyn MuxBackend> {
        Box::new(match workspace {
            Some(workspace) => NativeBackend::for_workspace(workspace),
            None => NativeBackend::new(),
        })
    }
}

impl MuxAppBackendProvider for NativeProvider {
    fn app_policy(&self) -> MuxAppBackendPolicy {
        MuxAppBackendPolicy {
            panes: PaneBehavior {
                topology: PaneTopology::ProcessLocal,
                cache_terminals: true,
                resize_cached_terminals: false,
            },
            progress: TerminalProgressPolicy::TerminalOsc,
            persisted_sessions: PersistedSessionPolicy::Immediate,
            generated_session_names: GeneratedSessionNamePolicy::Reconcile,
            terminal_residency: TerminalResidency::WorkspaceShared,
            selection_publication: SelectionPublicationPolicy::Direct,
        }
    }

    fn build_pane_policy(&self, _config: &MuxBindingConfig) -> Box<dyn BackendPanePolicy> {
        Box::new(NativePanePolicy)
    }

    fn capabilities(&self, scope: MuxScope) -> BindingCapabilityDescriptor {
        native_capabilities(scope)
    }
}

bootty_mux::register_mux_backend!(NativeProvider);
bootty_mux::register_mux_app_backend!(NativeProvider);

pub fn link() {}
