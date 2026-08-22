use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::controller::MuxScope;

pub const BINDING_CAPABILITY_DESCRIPTOR_VERSION: u16 = 1;

/// Backend-neutral operations a binding may expose.
#[derive(Clone, Copy, Debug, Deserialize, Hash, Ord, PartialOrd, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingOperation {
    ActivateWindow,
    CreateWindow,
    RenameWindow,
    NavigateWindow,
    MoveWindow,
    SplitPane,
    NavigatePane,
    ClosePane,
    TogglePaneZoom,
    CreateProjectSession,
    CreateWorktreeSession,
    RenameSession,
    DitchSession,
    StampSession,
}

/// Versioned capability declaration for one binding runtime.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct BindingCapabilityDescriptor {
    version: u16,
    scope: MuxScope,
    operations: BTreeSet<BindingOperation>,
}

impl BindingCapabilityDescriptor {
    pub fn new(scope: MuxScope, operations: impl IntoIterator<Item = BindingOperation>) -> Self {
        Self {
            version: BINDING_CAPABILITY_DESCRIPTOR_VERSION,
            scope,
            operations: operations.into_iter().collect(),
        }
    }

    pub fn version(&self) -> u16 {
        self.version
    }

    pub fn scope(&self) -> MuxScope {
        self.scope
    }

    pub fn operations(&self) -> impl Iterator<Item = BindingOperation> + '_ {
        self.operations.iter().copied()
    }

    pub fn supports(&self, operation: BindingOperation) -> bool {
        self.operations.contains(&operation)
    }

    pub fn request(&self, operation: BindingOperation) -> BindingOperationRequest {
        BindingOperationRequest {
            descriptor_version: self.version,
            scope: self.scope,
            operation,
        }
    }

    /// Runs only after the request matches this descriptor and the operation is currently available.
    pub fn invoke<T>(
        &self,
        request: BindingOperationRequest,
        availability: BindingOperationAvailability,
        operation: impl FnOnce() -> T,
    ) -> BindingOperationOutcome<T> {
        if request.descriptor_version != self.version || request.scope != self.scope {
            return BindingOperationOutcome::Stale;
        }
        if !self.supports(request.operation) {
            return BindingOperationOutcome::Unsupported;
        }
        if availability == BindingOperationAvailability::Unavailable {
            return BindingOperationOutcome::Unavailable;
        }
        BindingOperationOutcome::Supported(operation())
    }
}

/// An operation request bound to the descriptor version and binding scope that produced it.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct BindingOperationRequest {
    pub descriptor_version: u16,
    pub scope: MuxScope,
    pub operation: BindingOperation,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingOperationAvailability {
    Available,
    Unavailable,
}

/// Typed result of attempting an operation advertised by a binding descriptor.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingOperationOutcome<T> {
    Supported(T),
    Unsupported,
    Unavailable,
    Stale,
}
