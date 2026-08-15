use std::{
    fs,
    sync::{
        Arc, Barrier,
        mpsc::{self, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use bootty_command::{
    AppCommandReceiver, AppCommandSender, Caller, CommandCancellation, CommandInvocation,
    CommandOutcome, app_command_channel as command_channel,
};
use bootty_extension::{
    EVENT_QUEUE_LIMIT, ExtensionCatalog, ExtensionEventReceiver, ExtensionHost,
    ExtensionInvocationSender, ModuleIdentity, event_queue,
};

const VERSION_ONE: &str = r#"
bootty.events.register("probe.changed")
bootty.ui.register({ id = "probe.status", placement = "status" }, function()
    return { text = "version 1" }
end)
bootty.commands.register({ id = "probe.echo", title = "Echo" }, function()
    return { version = 1 }
end)
"#;

const VERSION_TWO: &str = r#"
bootty.events.register("probe.changed")
bootty.ui.register({ id = "probe.status", placement = "status" }, function()
    return { text = "version 2" }
end)
bootty.commands.register({ id = "probe.echo", title = "Echo" }, function()
    return { version = 2 }
end)
"#;

const RUNAWAY: &str = r#"
bootty.events.register("probe.changed")
bootty.ui.register({ id = "probe.status", placement = "status" }, function()
    return { text = "version 3" }
end)
bootty.commands.register({ id = "probe.echo", title = "Echo" }, function()
    bootty.events.publish("probe.changed", {})
    while true do end
end)
"#;

const DEADLINE_RUNAWAY: &str = r#"
bootty.commands.register({ id = "probe.echo", title = "Echo" }, function()
    while true do end
end)
"#;

const VERSION_THREE: &str = r#"
bootty.events.register("probe.changed")
bootty.commands.register({ id = "probe.echo", title = "Echo" }, function()
    return { version = 3 }
end)
"#;

const REPLACEMENT: &str = r#"
bootty.commands.register({ id = "probe.echo", title = "Echo" }, function()
    return { version = 4 }
end)
"#;

fn app_command_channel(capacity: usize) -> (AppCommandSender, AppCommandReceiver) {
    command_channel(capacity, Arc::new(|| {}))
}

fn command_sender(catalog: &ExtensionCatalog) -> ExtensionInvocationSender {
    catalog
        .command("probe.echo")
        .map(|(_, sender)| sender)
        .expect("probe.echo command")
}

fn invoke(sender: &ExtensionInvocationSender, limit: Duration) -> CommandOutcome {
    sender
        .invoke(
            CommandInvocation::new("probe.echo", Vec::new(), Caller::Socket),
            Instant::now() + limit,
            CommandCancellation::new(),
        )
        .recv_timeout(limit + Duration::from_secs(1))
        .expect("extension command outcome")
}

fn outcome(sender: &ExtensionInvocationSender, limit: Duration) -> mpsc::Receiver<CommandOutcome> {
    sender.invoke(
        CommandInvocation::new("probe.echo", Vec::new(), Caller::Socket),
        Instant::now() + limit,
        CommandCancellation::new(),
    )
}

fn wait_for_event(receiver: &ExtensionEventReceiver) -> bootty_extension::ExtensionEventRequest {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match receiver.try_recv() {
            Ok(request) => return request,
            Err(TryRecvError::Empty) if Instant::now() < deadline => thread::yield_now(),
            Err(TryRecvError::Empty) => panic!("extension event was not published"),
            Err(TryRecvError::Disconnected) => panic!("extension event receiver disconnected"),
        }
    }
}

fn surface_text(catalog: &ExtensionCatalog, module: &str, surface: &str) -> String {
    catalog
        .surfaces()
        .into_iter()
        .find(|published| {
            published.module == module && published.snapshot.declaration.id == surface
        })
        .expect("published extension surface")
        .snapshot
        .items[0]
        .text
        .clone()
}

fn success_version(version: u64) -> CommandOutcome {
    CommandOutcome::Success {
        value: serde_json::json!({"version": version}),
        warnings: Vec::new(),
    }
}

#[test]
fn local_module_generation_replaces_atomically_and_retires_runaway_handlers() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    let module = directory.path().join("probe.luau");
    fs::write(&module, VERSION_ONE).expect("write first module generation");
    let catalog = Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4);
    let (events, event_receiver) = event_queue();
    let mut host = ExtensionHost::load(
        directory.path(),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        events,
    );
    let started = Instant::now();

    assert_eq!(
        invoke(&command_sender(&catalog), Duration::from_secs(1)),
        success_version(1)
    );
    assert!(catalog.topics().contains("probe.changed"));
    assert_eq!(
        surface_text(&catalog, "probe.luau", "probe.status"),
        "version 1"
    );
    let first = command_sender(&catalog);

    fs::write(&module, VERSION_TWO).expect("write second module generation");
    host.refresh(started + Duration::from_secs(1));
    assert_eq!(
        invoke(&command_sender(&catalog), Duration::from_secs(1)),
        success_version(2)
    );
    assert_eq!(
        surface_text(&catalog, "probe.luau", "probe.status"),
        "version 2"
    );
    assert!(matches!(
        invoke(&first, Duration::from_secs(1)),
        CommandOutcome::Failed { code, .. } if code == "stale_extension_generation"
    ));

    fs::write(
        &module,
        r#"
bootty.events.register("probe.partial")
bootty.ui.register({ id = "probe.partial", placement = "sidebar" }, function()
    return { text = "partial" }
end)
bootty.commands.register({ id = "probe.partial", title = "Partial" }, function() end)
this is not valid luau
"#,
    )
    .expect("write invalid module generation");
    host.refresh(started + Duration::from_secs(2));
    assert_eq!(
        invoke(&command_sender(&catalog), Duration::from_secs(1)),
        success_version(2)
    );
    assert!(catalog.describe("probe.partial").is_none());
    assert!(catalog.topics().contains("probe.changed"));
    assert_eq!(
        surface_text(&catalog, "probe.luau", "probe.status"),
        "version 2"
    );

    fs::write(&module, RUNAWAY).expect("write runaway module generation");
    host.refresh(started + Duration::from_secs(3));
    let runaway = command_sender(&catalog);
    let pending = outcome(&runaway, Duration::from_secs(5));
    let event = wait_for_event(&event_receiver);
    event
        .response
        .send(Ok(()))
        .expect("release runaway handler");

    fs::write(&module, VERSION_THREE).expect("write replacement module generation");
    host.refresh(started + Duration::from_secs(4));
    assert!(matches!(
        pending
            .recv_timeout(Duration::from_millis(300))
            .expect("retired outcome"),
        CommandOutcome::Failed { code, .. } if code == "stale_extension_generation"
    ));
    assert_eq!(
        invoke(&command_sender(&catalog), Duration::from_secs(1)),
        success_version(3)
    );

    let third = command_sender(&catalog);
    fs::remove_file(&module).expect("remove extension module");
    host.refresh(started + Duration::from_secs(5));
    assert!(catalog.describe("probe.echo").is_none());
    assert!(!catalog.topics().contains("probe.changed"));
    assert!(matches!(
        invoke(&third, Duration::from_secs(1)),
        CommandOutcome::Failed { code, .. } if code == "stale_extension_generation"
    ));
}

#[test]
fn runaway_handler_obeys_the_invocation_deadline() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    fs::write(directory.path().join("probe.luau"), DEADLINE_RUNAWAY)
        .expect("write runaway module generation");
    let catalog = Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4);
    let _host = ExtensionHost::load(
        directory.path(),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );

    assert!(matches!(
        invoke(&command_sender(&catalog), Duration::from_millis(50)),
        CommandOutcome::Failed { code, .. } if code == "deadline_exceeded"
    ));
}

#[test]
fn invocation_queue_is_bounded_and_reports_extension_busy() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    fs::write(directory.path().join("probe.luau"), RUNAWAY).expect("write runaway module");
    let catalog = Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4);
    let (events, event_receiver) = event_queue();
    let host = ExtensionHost::load(
        directory.path(),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        events,
    );
    let handler = command_sender(&catalog);
    let active = outcome(&handler, Duration::from_secs(5));
    let event = wait_for_event(&event_receiver);
    event
        .response
        .send(Ok(()))
        .expect("release runaway handler");

    let responses = (0..128)
        .map(|_| outcome(&handler, Duration::from_secs(5)))
        .collect::<Vec<_>>();
    let busy =
        responses
            .iter()
            .enumerate()
            .find_map(|(index, response)| match response.try_recv() {
                Ok(CommandOutcome::Failed { code, .. }) if code == "extension_busy" => Some(index),
                Ok(other) => panic!("unexpected immediate extension outcome: {other:?}"),
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => None,
            });
    assert!(
        busy.is_some(),
        "invocation queue accepted more than its bound"
    );

    drop(host);
    drop(active);
    drop(responses);
}

#[test]
fn queued_invocations_are_answered_when_the_generation_is_dropped() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    fs::write(directory.path().join("probe.luau"), RUNAWAY).expect("write runaway module");
    let catalog = Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4);
    let (events, event_receiver) = event_queue();
    let host = ExtensionHost::load(
        directory.path(),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        events,
    );
    let handler = command_sender(&catalog);
    let active = outcome(&handler, Duration::from_secs(5));
    let event = wait_for_event(&event_receiver);
    event
        .response
        .send(Ok(()))
        .expect("release runaway handler");
    let queued = outcome(&handler, Duration::from_secs(5));

    drop(host);
    assert!(matches!(
        queued
            .recv_timeout(Duration::from_secs(1))
            .expect("queued invocation shutdown"),
        CommandOutcome::Failed { code, .. }
            if code == "shutdown"
    ));
    assert!(matches!(
        active
            .recv_timeout(Duration::from_secs(1))
            .expect("active invocation shutdown"),
        CommandOutcome::Failed { code, .. }
            if code == "stale_extension_generation"
    ));
}

#[test]
fn queued_invocations_are_answered_when_the_generation_is_replaced() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    let module = directory.path().join("probe.luau");
    fs::write(&module, RUNAWAY).expect("write runaway module");
    let catalog = Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4);
    let (events, event_receiver) = event_queue();
    let mut host = ExtensionHost::load(
        directory.path(),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        events,
    );
    let handler = command_sender(&catalog);
    let active = outcome(&handler, Duration::from_secs(5));
    let event = wait_for_event(&event_receiver);
    event
        .response
        .send(Ok(()))
        .expect("release runaway handler");
    let queued = outcome(&handler, Duration::from_secs(5));

    fs::write(&module, REPLACEMENT).expect("write replacement module");
    host.refresh(Instant::now() + Duration::from_secs(1));

    assert!(matches!(
        queued
            .recv_timeout(Duration::from_secs(1))
            .expect("queued invocation replacement outcome"),
        CommandOutcome::Failed { code, .. }
            if code == "shutdown"
    ));
    assert!(matches!(
        active
            .recv_timeout(Duration::from_millis(300))
            .expect("active invocation replacement outcome"),
        CommandOutcome::Failed { code, .. }
            if code == "stale_extension_generation"
    ));
}

#[test]
fn dropping_the_event_receiver_answers_pending_publications_with_shutdown() {
    let (sender, receiver) = event_queue();
    let identity = ModuleIdentity::parse("probe.luau").expect("module identity");
    let barrier = Arc::new(Barrier::new(EVENT_QUEUE_LIMIT + 2));
    let (results, result_receiver) = mpsc::channel();
    let mut threads = Vec::new();
    for _ in 0..=EVENT_QUEUE_LIMIT {
        let sender = sender.clone();
        let barrier = Arc::clone(&barrier);
        let results = results.clone();
        let identity = identity.clone();
        threads.push(thread::spawn(move || {
            barrier.wait();
            let result = sender.publish(
                identity,
                1,
                "probe.changed".to_owned(),
                serde_json::json!({}),
                Instant::now() + Duration::from_secs(5),
                &CommandCancellation::new(),
            );
            results.send(result).expect("publication result receiver");
        }));
    }
    barrier.wait();

    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("full publication result"),
        Err("extension event queue is full".to_owned())
    );
    drop(receiver);
    for _ in 0..EVENT_QUEUE_LIMIT {
        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("shutdown publication result"),
            Err("extension event queue shut down".to_owned())
        );
    }
    for thread in threads {
        thread.join().expect("publication thread");
    }
}
