use std::{collections::HashMap, path::Path, sync::Arc};

use anyhow::{Result, bail};
use bootty_mux_model::{MuxBackendKind, MuxBindingConfig};
#[cfg(feature = "app")]
use strum::IntoEnumIterator;

use crate::backend::MuxBackend;
#[cfg(feature = "app")]
use crate::command::MuxCommand;
#[cfg(feature = "app")]
use crate::{
    capability::{
        BindingCapabilityDescriptor, BindingOperationAvailability, BindingOperationOutcome,
    },
    controller::SpaceId,
    terminal::BackendPanePolicy,
};

#[cfg(feature = "app")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneTopology {
    ProcessLocal,
    BackendReconciled,
    Attach,
}

#[cfg(feature = "app")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneBehavior {
    pub topology: PaneTopology,
    pub cache_terminals: bool,
    pub resize_cached_terminals: bool,
}

#[cfg(feature = "app")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalProgressPolicy {
    TerminalOsc,
    BackendSnapshot,
}

#[cfg(feature = "app")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistedSessionPolicy {
    Immediate,
    AfterEmptyInitialSnapshot,
    Never,
}

#[cfg(feature = "app")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeneratedSessionNamePolicy {
    Reconcile,
    PreserveBackend,
}

#[cfg(feature = "app")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalResidency {
    WorkspaceShared,
    BindingScoped,
}

#[cfg(feature = "app")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionPublicationPolicy {
    Direct,
    PersistBeforePublish,
}

#[cfg(feature = "app")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MuxAppBackendPolicy {
    pub panes: PaneBehavior,
    pub progress: TerminalProgressPolicy,
    pub persisted_sessions: PersistedSessionPolicy,
    pub generated_session_names: GeneratedSessionNamePolicy,
    pub terminal_residency: TerminalResidency,
    pub selection_publication: SelectionPublicationPolicy,
}

/// Selects how the controller invokes a provider.
///
/// CallerThread providers own their command lifecycle in the controller thread.
/// WorkerThread providers run through the controller's worker path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MuxCommandDispatch {
    CallerThread,
    #[default]
    WorkerThread,
}

/// One complete backend implementation.
pub trait MuxBackendProvider: Send + Sync {
    fn kind(&self) -> MuxBackendKind;

    fn command_dispatch(&self) -> MuxCommandDispatch;

    fn build_backend(
        &self,
        config: &MuxBindingConfig,
        workspace: Option<&Path>,
    ) -> Box<dyn MuxBackend>;
}

#[cfg(feature = "app")]
pub trait MuxAppBackendProvider: MuxBackendProvider {
    fn build_pane_policy(&self, config: &MuxBindingConfig) -> Box<dyn BackendPanePolicy>;

    fn app_policy(&self) -> MuxAppBackendPolicy;

    fn capabilities(&self, scope: SpaceId) -> BindingCapabilityDescriptor;
}

#[derive(Clone)]
pub struct MuxBackendEntry {
    core: Arc<dyn MuxBackendProvider>,
    #[cfg(feature = "app")]
    app: Option<Arc<dyn MuxAppBackendProvider>>,
}

impl MuxBackendEntry {
    #[cfg(feature = "app")]
    pub fn from_app_provider<P>(provider: Arc<P>) -> Self
    where
        P: MuxAppBackendProvider + 'static,
    {
        let core: Arc<dyn MuxBackendProvider> = provider.clone();
        let app: Arc<dyn MuxAppBackendProvider> = provider;
        Self {
            core,
            app: Some(app),
        }
    }

    pub fn from_core_provider(provider: Arc<dyn MuxBackendProvider>) -> Self {
        Self {
            core: provider,
            #[cfg(feature = "app")]
            app: None,
        }
    }
}

pub struct MuxBackendRegistration {
    pub constructor: fn() -> MuxBackendEntry,
}

inventory::collect!(MuxBackendRegistration);

#[cfg(feature = "app")]
#[macro_export]
macro_rules! register_mux_backend {
    ($provider:expr) => {
        inventory::submit! {
            $crate::provider::MuxBackendRegistration {
                constructor: || {
                    $crate::provider::MuxBackendEntry::from_app_provider(
                        std::sync::Arc::new($provider),
                    )
                },
            }
        }
    };
}

#[cfg(not(feature = "app"))]
#[macro_export]
macro_rules! register_mux_backend {
    ($provider:expr) => {
        inventory::submit! {
            $crate::provider::MuxBackendRegistration {
                constructor: || {
                    $crate::provider::MuxBackendEntry::from_core_provider(
                        std::sync::Arc::new($provider),
                    )
                },
            }
        }
    };
}

#[derive(Clone)]
pub struct MuxBackendRegistry {
    providers: Arc<HashMap<MuxBackendKind, MuxBackendEntry>>,
}

impl MuxBackendRegistry {
    pub fn collect(required: impl IntoIterator<Item = MuxBackendKind>) -> Result<Self> {
        Self::from_entries(
            inventory::iter::<MuxBackendRegistration>
                .into_iter()
                .map(|registration| (registration.constructor)()),
            required,
            cfg!(feature = "app"),
        )
    }

    pub fn from_core_providers(
        providers: impl IntoIterator<Item = Arc<dyn MuxBackendProvider>>,
        required: impl IntoIterator<Item = MuxBackendKind>,
    ) -> Result<Self> {
        Self::from_entries(
            providers
                .into_iter()
                .map(MuxBackendEntry::from_core_provider),
            required,
            false,
        )
    }

    #[cfg(feature = "app")]
    pub fn from_app_providers<P>(
        providers: impl IntoIterator<Item = Arc<P>>,
        required: impl IntoIterator<Item = MuxBackendKind>,
    ) -> Result<Self>
    where
        P: MuxAppBackendProvider + 'static,
    {
        Self::from_entries(
            providers
                .into_iter()
                .map(MuxBackendEntry::from_app_provider),
            required,
            true,
        )
    }

    fn from_entries(
        entries: impl IntoIterator<Item = MuxBackendEntry>,
        required: impl IntoIterator<Item = MuxBackendKind>,
        _require_app: bool,
    ) -> Result<Self> {
        let mut by_kind = HashMap::new();
        for entry in entries {
            let kind = entry.core.kind();
            #[cfg(feature = "app")]
            if _require_app && entry.app.is_none() {
                bail!("missing app mux backend provider for {kind:?}")
            }
            if by_kind.insert(kind, entry).is_some() {
                bail!("duplicate mux backend provider for {kind:?}")
            }
        }
        for kind in required {
            if !by_kind.contains_key(&kind) {
                bail!("missing mux backend provider for {kind:?}")
            }
        }
        Ok(Self {
            providers: Arc::new(by_kind),
        })
    }

    pub fn selected_kind(&self, config: &MuxBindingConfig) -> MuxBackendKind {
        selected_backend(config)
    }

    pub fn build_backend(
        &self,
        config: &MuxBindingConfig,
        workspace: Option<&Path>,
    ) -> Box<dyn MuxBackend> {
        self.build_backend_for_kind(self.selected_kind(config), config, workspace)
    }

    pub fn build_backend_for_kind(
        &self,
        kind: MuxBackendKind,
        config: &MuxBindingConfig,
        workspace: Option<&Path>,
    ) -> Box<dyn MuxBackend> {
        self.providers
            .get(&kind)
            .expect("validated mux backend registry lost a provider")
            .core
            .build_backend(config, workspace)
    }

    pub fn command_dispatch(&self, config: &MuxBindingConfig) -> MuxCommandDispatch {
        let kind = self.selected_kind(config);
        self.providers
            .get(&kind)
            .expect("validated mux backend registry lost a provider")
            .core
            .command_dispatch()
    }

    #[cfg(feature = "app")]
    pub fn desktop() -> Result<Self> {
        Self::collect(MuxBackendKind::iter())
    }

    #[cfg(feature = "app")]
    pub fn build_pane_policy(&self, config: &MuxBindingConfig) -> Box<dyn BackendPanePolicy> {
        let kind = self.selected_kind(config);
        self.providers
            .get(&kind)
            .expect("validated mux backend registry lost a provider")
            .app
            .as_ref()
            .expect("validated mux backend registry lost app policy")
            .build_pane_policy(config)
    }

    #[cfg(feature = "app")]
    pub fn app_policy(&self, config: &MuxBindingConfig) -> MuxAppBackendPolicy {
        let kind = self.selected_kind(config);
        self.providers
            .get(&kind)
            .expect("validated mux backend registry lost a provider")
            .app
            .as_ref()
            .expect("validated mux backend registry lost app policy")
            .app_policy()
    }

    #[cfg(feature = "app")]
    pub fn capabilities(
        &self,
        config: &MuxBindingConfig,
        scope: SpaceId,
    ) -> BindingCapabilityDescriptor {
        let kind = self.selected_kind(config);
        self.providers
            .get(&kind)
            .expect("validated mux backend registry lost a provider")
            .app
            .as_ref()
            .expect("validated mux backend registry lost app policy")
            .capabilities(scope)
    }

    #[cfg(feature = "app")]
    pub fn execute_checked(
        &self,
        config: &MuxBindingConfig,
        scope: SpaceId,
        backend: &mut dyn MuxBackend,
        command: MuxCommand,
    ) -> BindingOperationOutcome<Result<()>> {
        let descriptor = self.capabilities(config, scope);
        descriptor.invoke(
            descriptor.request(command.operation()),
            BindingOperationAvailability::Available,
            || backend.execute(command),
        )
    }
}

fn resolve_backend(backend: MuxBackendKind, remote: bool, windows: bool) -> MuxBackendKind {
    if windows && backend == MuxBackendKind::Tmux && !remote {
        return MuxBackendKind::Native;
    }
    backend
}

pub fn selected_backend(config: &MuxBindingConfig) -> MuxBackendKind {
    resolve_backend(config.backend, config.remote.is_some(), cfg!(windows))
}
