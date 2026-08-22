use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Instant,
};

use bootty_command::{
    AppCommandRequest, AppCommandSendError, Caller, CommandCancellation, CommandInvocation,
    CommandOutcome, CommandTarget, ResourceKind, app_command_channel,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use proptest_derive::Arbitrary;
use serde_json::json;

#[derive(Debug, Arbitrary)]
struct InvocationModel {
    command: String,
    #[proptest(strategy = "prop::collection::vec(\"[^\\\\p{C}]{0,24}\", 0..5)")]
    arguments: Vec<String>,
    #[proptest(
        strategy = "prop_oneof![Just(Caller::CommandPalette), Just(Caller::Keybinding), Just(Caller::BuiltinKeybinding), Just(Caller::Cli), Just(Caller::Socket), Just(Caller::Luau), Just(Caller::Internal)]"
    )]
    caller: Caller,
    #[proptest(
        strategy = "prop::option::of((prop_oneof![Just(ResourceKind::Instance), Just(ResourceKind::ApplicationWindow), Just(ResourceKind::Binding), Just(ResourceKind::Session), Just(ResourceKind::MuxWindow), Just(ResourceKind::Pane), Just(ResourceKind::Terminal)], any::<u64>()))"
    )]
    target: Option<(ResourceKind, u64)>,
}

proptest! {
    /// Every invocation value round-trips through its documented JSON representation.
    #[test]
    fn invocation_json_is_lossless(model in any::<InvocationModel>()) {
        let target = model.target.as_ref().map(|(kind, generation)| CommandTarget {
            kind: *kind,
            handle: "opaque".to_owned(),
            generation: *generation,
        });
        let invocation = CommandInvocation {
            command: model.command.clone(), arguments: model.arguments.clone(), caller: model.caller,
            target, confirmation: None,
        };
        let mut expected = json!({"command": model.command, "caller": model.caller});
        if !model.arguments.is_empty() { expected["arguments"] = json!(model.arguments); }
        if let Some((kind, generation)) = model.target {
            expected["target"] = json!({"kind": kind, "handle": "opaque", "generation": generation.to_string()});
        }
        let encoded = serde_json::to_value(&invocation).unwrap();
        assert_eq!(encoded, expected);
        prop_assert_eq!(serde_json::from_value::<CommandInvocation>(encoded).unwrap(), invocation);
    }

    /// For every mailbox capacity, exactly that many requests are accepted and wake the owner;
    /// the next request is rejected, and the transport-bound caller replaces forged input.
    #[test]
    fn mailbox_enforces_capacity_and_transport_identity(capacity in 1usize..16) {
        let wakes = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&wakes);
        let (sender, receiver) = app_command_channel(
            capacity, Arc::new(move || { observed.fetch_add(1, Ordering::Relaxed); })
        );
        let bound = sender.for_caller(Caller::Socket);
        for _ in 0..capacity {
            let (request, _) = request(Caller::Cli);
            prop_assert_eq!(bound.try_send(request), Ok(()));
        }
        let (overflow, _) = request(Caller::Cli);
        prop_assert_eq!(bound.try_send(overflow), Err(AppCommandSendError::Overloaded));
        let first = receiver.try_recv().unwrap();
        prop_assert_eq!(first.invocation.caller, Caller::Socket);
        prop_assert_eq!(wakes.load(Ordering::Relaxed), capacity);
    }
}

#[test]
fn cancellation_has_one_winner_before_execution_starts() {
    let started = CommandCancellation::new();
    assert!(started.try_start());
    assert!(!started.cancel());

    let cancelled = CommandCancellation::new();
    assert!(cancelled.cancel());
    assert!(!cancelled.try_start());
}

#[test]
fn dropping_the_receiver_fails_pending_and_future_requests() {
    let (sender, receiver) = app_command_channel(1, Arc::new(|| {}));
    let bound = sender.for_caller(Caller::Internal);
    let (pending, response) = request(Caller::Internal);
    bound.try_send(pending).expect("enqueue pending command");
    drop(receiver);

    assert_eq!(
        response.recv().expect("receive shutdown outcome"),
        CommandOutcome::Failed {
            code: "shutdown".into(),
            message: "application command channel shut down".into(),
        }
    );
    let (request, _) = request(Caller::Internal);
    assert_eq!(bound.try_send(request), Err(AppCommandSendError::Shutdown));
}

#[test]
fn success_outcome_uses_the_documented_wire_shape() {
    assert_eq!(
        serde_json::to_value(CommandOutcome::success()).expect("serialize successful outcome"),
        json!({"status": "success", "value": null})
    );
}

fn request(caller: Caller) -> (AppCommandRequest, mpsc::Receiver<CommandOutcome>) {
    let (response, receiver) = mpsc::channel();
    (
        AppCommandRequest {
            invocation: CommandInvocation::new("test", Vec::new(), caller),
            deadline: Instant::now(),
            cancellation: CommandCancellation::new(),
            response,
        },
        receiver,
    )
}
