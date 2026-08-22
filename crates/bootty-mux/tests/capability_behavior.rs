use std::collections::BTreeSet;

use bootty_mux::{
    capability::{
        BINDING_CAPABILITY_DESCRIPTOR_VERSION, BindingCapabilityDescriptor, BindingOperation,
        BindingOperationAvailability, BindingOperationOutcome,
    },
    controller::SpaceId,
    snapshot::{MuxSnapshot, MuxSnapshotDisposition},
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use static_assertions::{assert_impl_all, const_assert_eq};

const_assert_eq!(BINDING_CAPABILITY_DESCRIPTOR_VERSION, 1);
assert_impl_all!(BindingOperation: Copy, Ord, Send, Sync);

const OPERATIONS: [BindingOperation; 14] = [
    BindingOperation::ActivateWindow,
    BindingOperation::CreateWindow,
    BindingOperation::RenameWindow,
    BindingOperation::NavigateWindow,
    BindingOperation::MoveWindow,
    BindingOperation::SplitPane,
    BindingOperation::NavigatePane,
    BindingOperation::ClosePane,
    BindingOperation::TogglePaneZoom,
    BindingOperation::CreateProjectSession,
    BindingOperation::CreateWorktreeSession,
    BindingOperation::RenameSession,
    BindingOperation::DitchSession,
    BindingOperation::StampSession,
];

fn operation_token(operation: BindingOperation) -> &'static str {
    match operation {
        BindingOperation::ActivateWindow => "activate_window",
        BindingOperation::CreateWindow => "create_window",
        BindingOperation::RenameWindow => "rename_window",
        BindingOperation::NavigateWindow => "navigate_window",
        BindingOperation::MoveWindow => "move_window",
        BindingOperation::SplitPane => "split_pane",
        BindingOperation::NavigatePane => "navigate_pane",
        BindingOperation::ClosePane => "close_pane",
        BindingOperation::TogglePaneZoom => "toggle_pane_zoom",
        BindingOperation::CreateProjectSession => "create_project_session",
        BindingOperation::CreateWorktreeSession => "create_worktree_session",
        BindingOperation::RenameSession => "rename_session",
        BindingOperation::DitchSession => "ditch_session",
        BindingOperation::StampSession => "stamp_session",
    }
}

proptest! {
    /// Construction deduplicates/orders operations; JSON preserves the exact set, version, and scope.
    #[test]
    fn descriptors_preserve_set_and_wire_behavior(
        scope in any::<i64>(),
        operations in prop::collection::vec(prop::sample::select(&OPERATIONS[..]), 0..40),
    ) {
        let expected_set = operations.iter().copied().collect::<BTreeSet<_>>();
        let expected_wire = serde_json::json!({
            "version": 1,
            "scope": scope,
            "operations": expected_set.iter().copied().map(operation_token).collect::<Vec<_>>(),
        });
        let descriptor = BindingCapabilityDescriptor::new(SpaceId::from_persistence(scope), operations);
        let encoded = serde_json::to_value(&descriptor).expect("serialize descriptor");

        assert_eq!(encoded, expected_wire);
        prop_assert_eq!(serde_json::from_value::<BindingCapabilityDescriptor>(encoded).unwrap(), descriptor);
    }
}

#[test]
fn invocation_requires_current_scope_support_and_availability() {
    use BindingOperation::{RenameSession, SplitPane};
    use BindingOperationAvailability::{Available, Unavailable};
    use BindingOperationOutcome::{Stale, Supported, Unavailable as IsUnavailable, Unsupported};

    let descriptor = BindingCapabilityDescriptor::new(SpaceId::from_persistence(1), [SplitPane]);
    for (scope, version, operation, availability, expected, expected_calls) in [
        (1, 1, SplitPane, Available, Supported("ran"), 1),
        (1, 1, RenameSession, Available, Unsupported, 0),
        (1, 1, SplitPane, Unavailable, IsUnavailable, 0),
        (2, 1, SplitPane, Available, Stale, 0),
        (1, 2, SplitPane, Available, Stale, 0),
    ] {
        let mut request = descriptor.request(operation);
        request.scope = SpaceId::from_persistence(scope);
        request.descriptor_version = version;
        let mut calls = 0;
        let actual = descriptor.invoke(request, availability, || {
            calls += 1;
            "ran"
        });
        assert_eq!((actual, calls), (expected, expected_calls));
    }
}

#[rstest]
#[case(MuxSnapshotDisposition::Authoritative, serde_json::json!({"sessions": [], "active_session_id": null}))]
#[case(MuxSnapshotDisposition::Transient, serde_json::json!({"sessions": [], "active_session_id": null, "disposition": "Transient"}))]
fn snapshot_disposition_has_a_backward_compatible_wire_shape(
    #[case] disposition: MuxSnapshotDisposition,
    #[case] expected: serde_json::Value,
) {
    let snapshot = MuxSnapshot {
        disposition,
        ..MuxSnapshot::default()
    };
    let encoded = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(encoded, expected);
    assert_eq!(
        serde_json::from_value::<MuxSnapshot>(encoded)
            .unwrap()
            .disposition,
        disposition
    );
}
