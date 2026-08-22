use bootty_mux::capability::{
    BINDING_CAPABILITY_DESCRIPTOR_VERSION, BindingCapabilityDescriptor, BindingOperation,
    BindingOperationAvailability, BindingOperationOutcome,
};
use bootty_mux::controller::SpaceId;

fn scope(space_id: i64) -> SpaceId {
    SpaceId::from_persistence(space_id)
}

#[test]
fn supported_operation_runs_once() {
    let descriptor = BindingCapabilityDescriptor::new(scope(1), [BindingOperation::SplitPane]);
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
    let descriptor = BindingCapabilityDescriptor::new(scope(1), [BindingOperation::SplitPane]);
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

/// Two Spaces advertising the same operation still have separate descriptors, so a request issued
/// against one cannot be answered by the other.
#[test]
fn a_request_cannot_cross_spaces_even_when_both_offer_the_operation() {
    let first = BindingCapabilityDescriptor::new(scope(1), [BindingOperation::RenameSession]);
    let second = BindingCapabilityDescriptor::new(scope(2), [BindingOperation::RenameSession]);
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
    let descriptor = BindingCapabilityDescriptor::new(scope(1), [BindingOperation::SplitPane]);
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
        scope(2),
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
    assert_eq!(descriptor.scope(), scope(2));
    assert!(descriptor.supports(BindingOperation::CreateWindow));
    assert_eq!(descriptor.operations().count(), 1);
}
