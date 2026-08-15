use std::{collections::HashMap, path::Path, sync::Arc};

use anyhow::{Result, bail};
use bootty_mux_model::{MuxBackendKind, MuxBindingConfig};
use strum::IntoEnumIterator;

use crate::{backend::MuxBackend, command::MuxCommand};
#[cfg(feature = "app")]
use crate::{
    capability::{
        BindingCapabilityDescriptor, BindingOperationAvailability, BindingOperationOutcome,
    },
    controller::MuxScope,
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

    fn capabilities(&self, scope: MuxScope) -> BindingCapabilityDescriptor;
}

pub struct MuxBackendRegistration {
    pub constructor: fn() -> Arc<dyn MuxBackendProvider>,
}

inventory::collect!(MuxBackendRegistration);

#[cfg(feature = "app")]
pub struct MuxAppBackendRegistration {
    pub constructor: fn() -> Arc<dyn MuxAppBackendProvider>,
}

#[cfg(feature = "app")]
inventory::collect!(MuxAppBackendRegistration);

#[macro_export]
macro_rules! register_mux_backend {
    ($provider:expr) => {
        inventory::submit! {
            $crate::provider::MuxBackendRegistration {
                constructor: || std::sync::Arc::new($provider),
            }
        }
    };
}

#[cfg(feature = "app")]
#[macro_export]
macro_rules! register_mux_app_backend {
    ($provider:expr) => {
        inventory::submit! {
            $crate::provider::MuxAppBackendRegistration {
                constructor: || std::sync::Arc::new($provider),
            }
        }
    };
}

#[derive(Clone)]
pub struct MuxBackendRegistry {
    providers: Arc<HashMap<MuxBackendKind, Arc<dyn MuxBackendProvider>>>,
}

impl MuxBackendRegistry {
    pub fn collect(required: impl IntoIterator<Item = MuxBackendKind>) -> Result<Self> {
        Self::from_providers(
            inventory::iter::<MuxBackendRegistration>
                .into_iter()
                .map(|registration| (registration.constructor)()),
            required,
        )
    }

    pub fn from_providers(
        providers: impl IntoIterator<Item = Arc<dyn MuxBackendProvider>>,
        required: impl IntoIterator<Item = MuxBackendKind>,
    ) -> Result<Self> {
        let mut by_kind = HashMap::new();
        for provider in providers {
            let kind = provider.kind();
            if by_kind.insert(kind, provider).is_some() {
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
            .build_backend(config, workspace)
    }

    pub fn command_dispatch(&self, config: &MuxBindingConfig) -> MuxCommandDispatch {
        let kind = self.selected_kind(config);
        self.providers
            .get(&kind)
            .expect("validated mux backend registry lost a provider")
            .command_dispatch()
    }
}

#[cfg(feature = "app")]
#[derive(Clone)]
pub struct MuxAppBackendRegistry {
    core: MuxBackendRegistry,
    providers: Arc<HashMap<MuxBackendKind, Arc<dyn MuxAppBackendProvider>>>,
}

#[cfg(feature = "app")]
impl MuxAppBackendRegistry {
    pub fn collect(required: impl IntoIterator<Item = MuxBackendKind>) -> Result<Self> {
        let required = required.into_iter().collect::<Vec<_>>();
        Self::from_providers(
            inventory::iter::<MuxBackendRegistration>
                .into_iter()
                .map(|registration| (registration.constructor)()),
            inventory::iter::<MuxAppBackendRegistration>
                .into_iter()
                .map(|registration| (registration.constructor)()),
            required,
        )
    }

    pub fn desktop() -> Result<Self> {
        Self::collect(MuxBackendKind::iter())
    }

    pub fn from_providers(
        core_providers: impl IntoIterator<Item = Arc<dyn MuxBackendProvider>>,
        app_providers: impl IntoIterator<Item = Arc<dyn MuxAppBackendProvider>>,
        required: impl IntoIterator<Item = MuxBackendKind>,
    ) -> Result<Self> {
        let required = required.into_iter().collect::<Vec<_>>();
        let core = MuxBackendRegistry::from_providers(core_providers, required.iter().copied())?;
        let mut by_kind = HashMap::new();
        for provider in app_providers {
            let kind = provider.kind();
            if !core.providers.contains_key(&kind) {
                bail!("app mux backend provider for {kind:?} has no core provider")
            }
            if by_kind.insert(kind, provider).is_some() {
                bail!("duplicate app mux backend provider for {kind:?}")
            }
        }
        for kind in required {
            if !by_kind.contains_key(&kind) {
                bail!("missing app mux backend provider for {kind:?}")
            }
        }
        Ok(Self {
            core,
            providers: Arc::new(by_kind),
        })
    }

    pub fn selected_kind(&self, config: &MuxBindingConfig) -> MuxBackendKind {
        self.core.selected_kind(config)
    }

    pub fn build_backend(
        &self,
        config: &MuxBindingConfig,
        workspace: Option<&Path>,
    ) -> Box<dyn MuxBackend> {
        self.core.build_backend(config, workspace)
    }

    pub fn build_backend_for_kind(
        &self,
        kind: MuxBackendKind,
        config: &MuxBindingConfig,
        workspace: Option<&Path>,
    ) -> Box<dyn MuxBackend> {
        self.core.build_backend_for_kind(kind, config, workspace)
    }

    pub fn command_dispatch(&self, config: &MuxBindingConfig) -> MuxCommandDispatch {
        self.core.command_dispatch(config)
    }

    pub fn build_pane_policy(&self, config: &MuxBindingConfig) -> Box<dyn BackendPanePolicy> {
        let kind = self.selected_kind(config);
        self.providers
            .get(&kind)
            .expect("validated app mux backend registry lost a provider")
            .build_pane_policy(config)
    }

    pub fn app_policy(&self, config: &MuxBindingConfig) -> MuxAppBackendPolicy {
        let kind = self.selected_kind(config);
        self.providers
            .get(&kind)
            .expect("validated app mux backend registry lost a provider")
            .app_policy()
    }

    pub fn capabilities(
        &self,
        config: &MuxBindingConfig,
        scope: MuxScope,
    ) -> BindingCapabilityDescriptor {
        let kind = self.selected_kind(config);
        self.providers
            .get(&kind)
            .expect("validated mux backend registry lost a provider")
            .capabilities(scope)
    }

    pub fn execute_checked(
        &self,
        config: &MuxBindingConfig,
        scope: MuxScope,
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
