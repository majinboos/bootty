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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{BindingId, SpaceId};

    fn scope(space_id: i64, binding_id: i64) -> MuxScope {
        MuxScope::new(
            SpaceId::from_persistence(space_id),
            BindingId::from_persistence(binding_id),
        )
    }

    #[test]
    fn supported_operation_runs_once() {
        let descriptor =
            BindingCapabilityDescriptor::new(scope(1, 1), [BindingOperation::SplitPane]);
        let mut calls = 0;

        let outcome = descriptor.invoke(
            descriptor.request(BindingOperation::SplitPane),
            BindingOperationAvailability::Available,
            || {
                calls += 1;
                "split"
            },
        );

        assert_eq!(outcome, BindingOperationOutcome::Supported("split"));
        assert_eq!(calls, 1);
    }

    #[test]
    fn unsupported_and_unavailable_operations_do_not_run() {
        let descriptor =
            BindingCapabilityDescriptor::new(scope(1, 1), [BindingOperation::SplitPane]);
        let mut calls = 0;

        let unsupported = descriptor.invoke(
            descriptor.request(BindingOperation::RenameSession),
            BindingOperationAvailability::Available,
            || calls += 1,
        );
        let unavailable = descriptor.invoke(
            descriptor.request(BindingOperation::SplitPane),
            BindingOperationAvailability::Unavailable,
            || calls += 1,
        );

        assert_eq!(unsupported, BindingOperationOutcome::Unsupported);
        assert_eq!(unavailable, BindingOperationOutcome::Unavailable);
        assert_eq!(calls, 0);
    }

    #[test]
    fn stale_request_cannot_cross_binding_scope_with_colliding_operation() {
        let first =
            BindingCapabilityDescriptor::new(scope(1, 1), [BindingOperation::RenameSession]);
        let second =
            BindingCapabilityDescriptor::new(scope(1, 2), [BindingOperation::RenameSession]);
        let mut calls = 0;

        let outcome = second.invoke(
            first.request(BindingOperation::RenameSession),
            BindingOperationAvailability::Available,
            || calls += 1,
        );

        assert_eq!(outcome, BindingOperationOutcome::Stale);
        assert_eq!(calls, 0);
    }

    #[test]
    fn mismatched_descriptor_version_is_stale_and_does_not_run() {
        let descriptor =
            BindingCapabilityDescriptor::new(scope(1, 1), [BindingOperation::SplitPane]);
        let mut request = descriptor.request(BindingOperation::SplitPane);
        request.descriptor_version += 1;
        let mut calls = 0;

        let outcome = descriptor.invoke(request, BindingOperationAvailability::Available, || {
            calls += 1
        });

        assert_eq!(outcome, BindingOperationOutcome::Stale);
        assert_eq!(calls, 0);
    }

    #[test]
    fn descriptor_round_trips_with_its_version_and_scope() {
        let descriptor = BindingCapabilityDescriptor::new(
            scope(2, 3),
            [
                BindingOperation::CreateWindow,
                BindingOperation::CreateWindow,
            ],
        );

        let encoded = serde_json::to_string(&descriptor).expect("serialize descriptor");
        let decoded: BindingCapabilityDescriptor =
            serde_json::from_str(&encoded).expect("deserialize descriptor");

        assert_eq!(decoded, descriptor);
        assert_eq!(descriptor.version(), BINDING_CAPABILITY_DESCRIPTOR_VERSION);
        assert!(descriptor.supports(BindingOperation::CreateWindow));
        assert_eq!(descriptor.operations().count(), 1);
    }
}
