use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

use bootty_command::{
    AppCommandRequest, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
    CommandTarget, ResourceKind, app_command_channel,
};

#[test]
fn serialization_preserves_public_command_shapes() {
    let target = CommandTarget {
        kind: ResourceKind::Pane,
        handle: "opaque".to_owned(),
        generation: u64::MAX,
    };
    let invocation = CommandInvocation {
        command: "close_surface".to_owned(),
        arguments: vec!["x".to_owned()],
        caller: Caller::Socket,
        target: Some(target),
        confirmation: None,
    };
    let json = serde_json::to_value(&invocation).expect("serialize invocation");
    assert_eq!(json["caller"], "socket");
    assert_eq!(json["target"]["generation"], u64::MAX.to_string());

    let outcome = serde_json::to_value(CommandOutcome::success()).expect("serialize outcome");
    assert_eq!(
        outcome,
        serde_json::json!({"status": "success", "value": null})
    );
}

#[test]
fn bound_sender_overwrites_untrusted_caller() {
    let wake_count = Arc::new(AtomicUsize::new(0));
    let wake_count_clone = wake_count.clone();
    let (sender, receiver) = app_command_channel(
        1,
        Arc::new(move || {
            wake_count_clone.fetch_add(1, Ordering::Relaxed);
        }),
    );
    sender
        .for_caller(Caller::Socket)
        .try_send(AppCommandRequest {
            invocation: CommandInvocation::new("test", Vec::new(), Caller::Cli),
            deadline: Instant::now(),
            cancellation: CommandCancellation::new(),
            response: std::sync::mpsc::channel().0,
        })
        .expect("send request");
    assert_eq!(
        receiver
            .try_recv()
            .expect("receive request")
            .invocation
            .caller,
        Caller::Socket
    );
    assert_eq!(wake_count.load(Ordering::Relaxed), 1);
}

#[test]
fn mailbox_reports_overload_and_drains_shutdown() {
    let (sender, receiver) = app_command_channel(1, Arc::new(|| {}));
    let bound = sender.for_caller(Caller::Internal);
    let request = || AppCommandRequest {
        invocation: CommandInvocation::new("test", Vec::new(), Caller::Internal),
        deadline: Instant::now(),
        cancellation: CommandCancellation::new(),
        response: std::sync::mpsc::channel().0,
    };
    bound.try_send(request()).expect("fill mailbox");
    assert_eq!(
        bound.try_send(request()),
        Err(bootty_command::AppCommandSendError::Overloaded)
    );
    receiver.try_recv().expect("drain first request");
    let response = std::sync::mpsc::channel();
    bound
        .try_send(AppCommandRequest {
            response: response.0,
            ..request()
        })
        .expect("replace request");
    drop(receiver);
    assert!(response.1.try_recv().is_ok());
}

#[test]
fn cancellation_has_pending_started_and_cancelled_transitions() {
    let pending = CommandCancellation::new();
    assert!(pending.try_start());
    assert!(!pending.cancel());
    assert!(!pending.is_cancelled());

    let cancellable = CommandCancellation::new();
    assert!(cancellable.cancel());
    assert!(cancellable.is_cancelled());
    assert!(!cancellable.try_start());
}
