#![cfg(unix)]

use std::{
    process::Command,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use bootty_command::{
    AppCommandReceiver, AppCommandRequest, AppCommandSender, Caller, CommandDescriptor,
    CommandOutcome, CompactSchema, MutationClass, app_command_channel as command_channel,
};
use bootty_control::{
    ControlCatalog, ControlPlane, ControlServer, RpcResponse, invoke_instance, running_instance,
};
use bootty_extension::{
    ExtensionCatalog, ExtensionGenerationCandidate, ExtensionGenerationToken, ModuleIdentity,
};
use serde_json::{Value, json};

const HELPER_ENV: &str = "BOOTTY_CONTROL_CONTRACTS_TEST_HELPER";

fn run_isolated(test_name: &str) {
    let runtime = tempfile::tempdir().expect("temporary runtime directory");
    let status = Command::new(std::env::current_exe().expect("current test executable"))
        .args(["--exact", test_name])
        .env(HELPER_ENV, "1")
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("RMUX_TMPDIR", runtime.path())
        .status()
        .expect("run isolated control contract");
    assert!(status.success());
}

fn app_command_channel(capacity: usize) -> (AppCommandSender, AppCommandReceiver) {
    command_channel(capacity, Arc::new(|| {}))
}

fn descriptor(id: &str, mutation: MutationClass) -> CommandDescriptor {
    CommandDescriptor {
        id: id.to_owned(),
        title: id.to_owned(),
        description: String::new(),
        mutation,
        arguments: CompactSchema::default(),
        target: None,
        palette: false,
    }
}

fn receive_request(receiver: &AppCommandReceiver) -> AppCommandRequest {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match receiver.try_recv() {
            Ok(request) => return request,
            Err(mpsc::TryRecvError::Empty) if Instant::now() < deadline => thread::yield_now(),
            Err(mpsc::TryRecvError::Empty) => panic!("control request did not arrive"),
            Err(mpsc::TryRecvError::Disconnected) => panic!("control request channel closed"),
        }
    }
}

fn response_result(response: RpcResponse) -> Value {
    assert!(
        response.error.is_none(),
        "unexpected RPC error: {:?}",
        response.error
    );
    response.result.expect("RPC result")
}

fn test_server(
    core: Vec<CommandDescriptor>,
) -> (
    ControlServer,
    Arc<ControlCatalog>,
    ControlPlane,
    AppCommandReceiver,
) {
    let extensions = Arc::new(ExtensionCatalog::default());
    let catalog = Arc::new(ControlCatalog::new(core, extensions));
    let plane = ControlPlane::default();
    let (sender, receiver) = app_command_channel(4);
    let server = ControlServer::spawn(
        "control-contracts",
        sender.for_caller(Caller::Socket),
        Arc::clone(&catalog),
        &plane,
    )
    .expect("start control server");
    (server, catalog, plane, receiver)
}

fn instance() -> bootty_control::InstanceDescriptor {
    running_instance()
        .expect("discover control server")
        .expect("control instance")
}

#[test]
fn json_rpc_server_lists_describes_and_invokes_commands() {
    run_isolated("json_rpc_server_lists_describes_and_invokes_commands_helper");
}

#[test]
fn json_rpc_server_lists_describes_and_invokes_commands_helper() {
    if std::env::var_os(HELPER_ENV).is_none() {
        return;
    }
    let (server, _catalog, _plane, receiver) =
        test_server(vec![descriptor("control.read", MutationClass::Read)]);
    let descriptor = instance();

    let system = response_result(
        invoke_instance(&descriptor, "system.describe", Value::Null).expect("describe protocol"),
    );
    assert_eq!(system["protocol"]["current"], json!(1));
    assert!(
        system["methods"]
            .as_array()
            .is_some_and(|methods| { methods.iter().any(|method| method == "command.invoke") })
    );

    let commands = response_result(
        invoke_instance(&descriptor, "command.list", Value::Null).expect("list commands"),
    );
    assert_eq!(commands[0]["id"], json!("control.read"));
    let described = response_result(
        invoke_instance(
            &descriptor,
            "command.describe",
            json!({"command": "control.read"}),
        )
        .expect("describe command"),
    );
    assert_eq!(described["mutation"], json!("read"));

    let request_descriptor = descriptor.clone();
    let invocation = thread::spawn(move || {
        invoke_instance(
            &request_descriptor,
            "command.invoke",
            json!({"invocation": {"command": "control.read", "caller": "socket"}}),
        )
        .expect("invoke command")
    });
    let request = receive_request(&receiver);
    assert_eq!(request.invocation.command, "control.read");
    assert_eq!(request.invocation.caller, Caller::Socket);
    request
        .response
        .send(CommandOutcome::Success {
            value: json!({"value": 42}),
            warnings: Vec::new(),
        })
        .expect("complete command");
    assert_eq!(
        response_result(invocation.join().expect("join RPC")),
        json!({
            "status": "success",
            "value": {"value": 42},
        })
    );

    drop(server);
}

#[test]
fn tasks_subscriptions_bounded_events_generations_and_cancellation_are_control_owned() {
    run_isolated(
        "tasks_subscriptions_bounded_events_generations_and_cancellation_are_control_owned_helper",
    );
}

#[test]
fn tasks_subscriptions_bounded_events_generations_and_cancellation_are_control_owned_helper() {
    if std::env::var_os(HELPER_ENV).is_none() {
        return;
    }
    let (server, catalog, plane, receiver) =
        test_server(vec![descriptor("control.read", MutationClass::Read)]);
    let descriptor = instance();

    let subscription = response_result(
        invoke_instance(
            &descriptor,
            "event.subscribe",
            json!({"topics": ["command.completed"]}),
        )
        .expect("subscribe command events"),
    )["subscription"]
        .as_str()
        .expect("subscription id")
        .to_owned();

    let task_descriptor = descriptor.clone();
    let task = thread::spawn(move || {
        invoke_instance(
            &task_descriptor,
            "command.invoke",
            json!({
                "detached": true,
                "invocation": {"command": "control.read", "caller": "socket"}
            }),
        )
        .expect("start detached command")
    });
    let request = receive_request(&receiver);
    let task_result = response_result(task.join().expect("join detached RPC"));
    let task_id = task_result["task"]["id"]
        .as_str()
        .expect("task id")
        .to_owned();
    assert_eq!(
        response_result(
            invoke_instance(&descriptor, "task.status", json!({"task": task_id}),)
                .expect("read task status"),
        )["task"]["state"]["status"],
        json!("running")
    );
    let cancelling = response_result(
        invoke_instance(&descriptor, "task.cancel", json!({"task": task_id})).expect("cancel task"),
    );
    assert_eq!(cancelling["task"]["state"]["status"], json!("cancelling"));
    assert!(request.cancellation.is_cancelled());
    drop(request.response);

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let status = response_result(
            invoke_instance(&descriptor, "task.status", json!({"task": task_id}))
                .expect("poll cancelled task"),
        );
        if status["task"]["state"]["status"] == json!("completed") {
            assert_eq!(status["task"]["state"]["outcome"]["code"], json!("-32003"));
            break;
        }
        assert!(Instant::now() < deadline, "cancelled task did not complete");
        thread::yield_now();
    }

    let events = response_result(
        invoke_instance(
            &descriptor,
            "event.subscribe",
            json!({"subscription": subscription, "cursor": 0}),
        )
        .expect("poll command events"),
    );
    assert_eq!(events["events"][0]["topic"], json!("command.completed"));

    let first_token = ExtensionGenerationToken::new();
    catalog
        .extensions()
        .publish_generation(ExtensionGenerationCandidate {
            identity: ModuleIdentity::parse("test.luau").expect("module identity"),
            generation: 1,
            token: first_token.clone(),
            commands: Vec::new(),
            topics: vec!["test.changed".to_owned()],
            surfaces: Vec::new(),
        })
        .expect("publish first generation");
    let extension_subscription = response_result(
        invoke_instance(
            &descriptor,
            "event.subscribe",
            json!({"topics": ["test.changed"]}),
        )
        .expect("subscribe extension events"),
    )["subscription"]
        .as_str()
        .expect("extension subscription id")
        .to_owned();
    for sequence in 0..65 {
        plane
            .publish_extension_event(
                &catalog,
                "test.luau",
                1,
                "test.changed",
                &json!({"sequence": sequence}),
            )
            .expect("publish bounded event");
    }
    catalog
        .extensions()
        .publish_generation(ExtensionGenerationCandidate {
            identity: ModuleIdentity::parse("test.luau").expect("module identity"),
            generation: 2,
            token: ExtensionGenerationToken::new(),
            commands: Vec::new(),
            topics: vec!["test.changed".to_owned()],
            surfaces: Vec::new(),
        })
        .expect("replace generation");
    assert!(!first_token.is_active());
    assert_eq!(
        plane.publish_extension_event(
            &catalog,
            "test.luau",
            1,
            "test.changed",
            &json!({"sequence": "stale"}),
        ),
        Err("extension event topic is not active".to_owned())
    );
    let response = invoke_instance(
        &descriptor,
        "event.subscribe",
        json!({"subscription": extension_subscription, "cursor": 0}),
    )
    .expect("poll bounded extension events");
    assert_eq!(response.error.expect("event rebase error").code, -32005);

    let shutdown_descriptor = descriptor.clone();
    let pending = thread::spawn(move || {
        invoke_instance(
            &shutdown_descriptor,
            "command.invoke",
            json!({
                "detached": true,
                "invocation": {"command": "control.read", "caller": "socket"}
            }),
        )
    });
    let request = receive_request(&receiver);
    drop(server);
    assert!(request.cancellation.is_cancelled());
    let _ = pending.join();
}
