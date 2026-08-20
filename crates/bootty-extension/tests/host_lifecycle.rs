use std::{
    fs,
    sync::Arc,
    time::{Duration, Instant},
};

use bootty_command::{
    AppCommandReceiver, AppCommandSender, Caller, CommandCancellation, CommandInvocation,
    CommandOutcome, app_command_channel as command_channel,
};
use bootty_extension::{
    ExtensionCatalog, ExtensionHost, ExtensionInvocationSender, ExtensionUiAction, MuxView,
    SessionReorder, SurfacePlacement, event_queue,
};

/// Wall-clock budget for every step that waits on a real extension worker.
///
/// A loaded parallel run can starve a Luau worker thread for seconds, so the
/// budget only has to be generous enough never to expire on a healthy run.
/// Logical `host.refresh` deadlines are separate and stay short on purpose.
const EXTENSION_BUDGET: Duration = Duration::from_secs(30);

const MIXED_GENERATION: &str = r#"
local version = __VERSION__
bootty.events.register("mixed.changed")
bootty.commands.register({ id = "mixed.version", title = "Version" }, function()
    return { version = version }
end)
for _, placement in ipairs({ "status", "sidebar", "session", "floating", "docked" }) do
    local current = placement
    local suffix = "initial"
    bootty.ui.register({ id = "mixed." .. current, placement = current }, function()
        return { text = current .. ":v" .. version .. ":" .. suffix }
    end, function(action, payload)
        suffix = action .. ":" .. payload.value
    end)
end
"#;

fn app_command_channel(capacity: usize) -> (AppCommandSender, AppCommandReceiver) {
    command_channel(capacity, Arc::new(|| {}))
}

#[test]
fn structured_storage_is_bounded_and_follows_the_module_path_identity() {
    let directory = tempfile::tempdir().expect("temporary config root");
    let extension_root = directory.path().join("extensions");
    fs::create_dir(&extension_root).expect("create extension root");
    let module = extension_root.join("counter.luau");
    fs::write(
        &module,
        r#"
bootty.commands.register({ id = "counter.increment", title = "Increment" }, function()
    local count = bootty.storage.get("count") or 0
    count = count + 1
    bootty.storage.set("count", count)
    return { count = count }
end)
bootty.commands.register({ id = "counter.oversized", title = "Oversized" }, function()
    bootty.storage.set("oversized", string.rep("x", 65537))
end)
bootty.commands.register({ id = "counter.handoff", title = "Handoff" }, function()
    bootty.storage.set("handoff", "committed")
end)
"#,
    )
    .expect("write storage module");
    let catalog = Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4);
    {
        let _host = ExtensionHost::load(
            &extension_root,
            Arc::clone(&catalog),
            sender.for_caller(Caller::Luau),
            event_queue().0,
        );
        assert_eq!(
            invoke_named(&catalog, "counter.increment", EXTENSION_BUDGET),
            CommandOutcome::Success {
                value: serde_json::json!({"count": 1}),
                warnings: Vec::new(),
            }
        );
        assert!(matches!(
            invoke_named(&catalog, "counter.oversized", EXTENSION_BUDGET),
            CommandOutcome::Failed { code, message }
                if code == "extension_failed"
                    && message.contains("extension storage value exceeds 65536 bytes")
        ));
    }

    let mut host = ExtensionHost::load(
        &extension_root,
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );
    assert_eq!(
        invoke_named(&catalog, "counter.increment", EXTENSION_BUDGET),
        CommandOutcome::Success {
            value: serde_json::json!({"count": 2}),
            warnings: Vec::new(),
        }
    );
    assert_eq!(
        fs::read_to_string(directory.path().join("extension-storage/counter.luau.json"))
            .expect("persisted extension storage"),
        r#"{"count":2}"#
    );

    fs::write(
        &module,
        r#"
while bootty.storage.get("handoff") == nil do end
bootty.commands.register({ id = "counter.increment", title = "Increment" }, function()
    local count = (bootty.storage.get("count") or 0) + 1
    bootty.storage.set("count", count)
    return { count = count, handoff = bootty.storage.get("handoff") }
end)
"#,
    )
    .expect("write handoff generation");
    std::thread::scope(|scope| {
        let refresh = scope.spawn(|| host.refresh(Instant::now() + Duration::from_secs(2)));
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(
            invoke_named(&catalog, "counter.handoff", EXTENSION_BUDGET),
            CommandOutcome::success()
        );
        refresh.join().expect("publish handoff generation");
    });
    assert_eq!(
        invoke_named(&catalog, "counter.increment", EXTENSION_BUDGET),
        CommandOutcome::Success {
            value: serde_json::json!({"count": 3, "handoff": "committed"}),
            warnings: Vec::new(),
        }
    );
    assert_eq!(
        fs::read_to_string(directory.path().join("extension-storage/counter.luau.json"))
            .expect("handoff storage"),
        r#"{"count":3,"handoff":"committed"}"#
    );
}

#[test]
fn one_generation_publishes_and_refreshes_every_surface_placement() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    fs::write(
        directory.path().join("surface.luau"),
        r#"
for _, placement in ipairs({ "status", "sidebar", "session", "floating", "docked" }) do
    bootty.ui.register({ id = placement, placement = placement }, function()
        return { text = placement .. ":" .. (bootty.session() or "none") }
    end)
end
"#,
    )
    .expect("write surface module");
    let catalog = Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4);
    let host = ExtensionHost::load(
        directory.path(),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );

    let initial = catalog
        .surfaces()
        .into_iter()
        .filter(|surface| surface.module == "surface.luau")
        .collect::<Vec<_>>();
    assert_eq!(initial.len(), 5);
    assert!(
        initial
            .iter()
            .all(|surface| surface.generation == initial[0].generation)
    );
    assert!(
        initial
            .iter()
            .all(|surface| surface.snapshot.items[0].text.ends_with(":none"))
    );

    host.update_mux(MuxView {
        session: Some("active-session".to_owned()),
        ..MuxView::default()
    });
    let deadline = Instant::now() + EXTENSION_BUDGET;
    loop {
        let refreshed = catalog
            .surfaces()
            .into_iter()
            .filter(|surface| surface.module == "surface.luau")
            .collect::<Vec<_>>();
        if refreshed
            .iter()
            .all(|surface| surface.snapshot.items[0].text.ends_with(":active-session"))
        {
            break;
        }
        assert!(Instant::now() < deadline, "surface refresh did not publish");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn commands_topics_surfaces_and_actions_switch_as_one_generation() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    let module = directory.path().join("mixed.luau");
    fs::write(&module, MIXED_GENERATION.replace("__VERSION__", "1"))
        .expect("first mixed generation");
    let catalog = Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4);
    let mut host = ExtensionHost::load(
        directory.path(),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );
    let started = Instant::now();
    let first = host
        .surfaces(SurfacePlacement::Status)
        .into_iter()
        .find(|surface| surface.module == "mixed.luau")
        .expect("first status surface");
    assert_eq!(
        invoke_named(&catalog, "mixed.version", EXTENSION_BUDGET),
        success_version(1)
    );
    assert!(catalog.topics().contains("mixed.changed"));

    fs::write(&module, MIXED_GENERATION.replace("__VERSION__", "2"))
        .expect("second mixed generation");
    host.refresh(started + Duration::from_secs(1));
    assert_eq!(
        invoke_named(&catalog, "mixed.version", EXTENSION_BUDGET),
        success_version(2)
    );
    let second = catalog
        .surfaces()
        .into_iter()
        .filter(|surface| surface.module == "mixed.luau")
        .collect::<Vec<_>>();
    assert_eq!(second.len(), 5);
    assert!(
        second
            .iter()
            .all(|surface| surface.generation == second[0].generation)
    );
    assert!(
        second
            .iter()
            .all(|surface| surface.snapshot.items[0].text.contains(":v2:initial"))
    );
    assert_eq!(
        host.submit_ui_action(ExtensionUiAction {
            module: first.module,
            generation: first.generation,
            surface: first.snapshot.declaration.id,
            action: "choose".to_owned(),
            payload: serde_json::json!({"value": "old"}),
        }),
        Err("extension generation is no longer active".to_owned())
    );

    for surface in &second {
        host.submit_ui_action(ExtensionUiAction {
            module: surface.module.clone(),
            generation: surface.generation,
            surface: surface.snapshot.declaration.id.clone(),
            action: "choose".to_owned(),
            payload: serde_json::json!({"value": "new"}),
        })
        .expect("current action");
    }
    let deadline = Instant::now() + EXTENSION_BUDGET;
    loop {
        let current = catalog
            .surfaces()
            .into_iter()
            .filter(|surface| surface.module == "mixed.luau")
            .collect::<Vec<_>>();
        if current
            .iter()
            .all(|surface| surface.snapshot.items[0].text.contains(":v2:choose:new"))
        {
            break;
        }
        assert!(Instant::now() < deadline, "surface actions did not publish");
        std::thread::sleep(Duration::from_millis(5));
    }

    fs::write(
        &module,
        format!(
            "{}\nthis is not valid luau",
            MIXED_GENERATION.replace("__VERSION__", "3")
        ),
    )
    .expect("invalid third generation");
    host.refresh(started + Duration::from_secs(2));
    assert_eq!(
        invoke_named(&catalog, "mixed.version", EXTENSION_BUDGET),
        success_version(2)
    );
    assert!(catalog.topics().contains("mixed.changed"));
    assert!(catalog.surfaces().into_iter().all(|surface| {
        surface.module != "mixed.luau" || surface.generation == second[0].generation
    }));
}

#[test]
fn runaway_surface_render_is_retired_without_late_publication() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    let module = directory.path().join("render.luau");
    fs::write(
        &module,
        r#"
bootty.ui.register({ id = "render", placement = "status" }, function()
    if bootty.session() == "runaway" then while true do end end
    return { text = "version 1" }
end)
"#,
    )
    .expect("first render generation");
    let catalog = Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4);
    let mut host = ExtensionHost::load(
        directory.path(),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );
    host.update_mux(MuxView {
        session: Some("runaway".to_owned()),
        ..MuxView::default()
    });
    std::thread::sleep(Duration::from_millis(10));
    fs::write(
        &module,
        r#"
bootty.ui.register({ id = "render", placement = "status" }, function()
    return { text = "version 2" }
end)
"#,
    )
    .expect("replacement render generation");
    let started = Instant::now();
    host.refresh(started + Duration::from_secs(1));
    assert!(started.elapsed() < Duration::from_millis(300));
    let deadline = Instant::now() + EXTENSION_BUDGET;
    loop {
        if surface_text(&catalog, "render.luau", "render") == "version 2" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "replacement surface did not publish"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(surface_text(&catalog, "render.luau", "render"), "version 2");
}

#[test]
fn a_surface_collision_rejects_the_complete_candidate_generation() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    fs::write(
        directory.path().join("a.luau"),
        r#"
bootty.ui.register({ id = "shared", placement = "status" }, function()
    return { text = "first" }
end)
"#,
    )
    .expect("first surface module");
    fs::write(
        directory.path().join("b.luau"),
        r#"
bootty.events.register("b.changed")
bootty.commands.register({ id = "b.command", title = "B" }, function() end)
bootty.ui.register({ id = "shared", placement = "status" }, function()
    return { text = "second" }
end)
"#,
    )
    .expect("colliding surface module");
    let catalog = Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4);
    let _host = ExtensionHost::load(
        directory.path(),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );

    assert_eq!(surface_text(&catalog, "a.luau", "shared"), "first");
    assert!(catalog.describe("b.command").is_none());
    assert!(!catalog.topics().contains("b.changed"));
    assert!(
        catalog
            .surfaces()
            .iter()
            .all(|surface| surface.module != "b.luau")
    );
}

#[test]
fn declarations_close_after_candidate_setup() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    fs::write(
        directory.path().join("late.luau"),
        r#"
bootty.ui.register({ id = "initial", placement = "status" }, function()
    return { text = "initial" }
end)
bootty.commands.register({ id = "late.try", title = "Late" }, function()
    local command_ok = pcall(function()
        bootty.commands.register({ id = "late.extra", title = "Extra" }, function() end)
    end)
    local topic_ok = pcall(function() bootty.events.register("late.extra") end)
    local surface_ok = pcall(function()
        bootty.ui.register({ id = "extra", placement = "sidebar" }, function() return "extra" end)
    end)
    return { command_ok = command_ok, topic_ok = topic_ok, surface_ok = surface_ok }
end)
"#,
    )
    .expect("late declaration module");
    let catalog = Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4);
    let _host = ExtensionHost::load(
        directory.path(),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );

    assert_eq!(
        invoke_named(&catalog, "late.try", EXTENSION_BUDGET),
        CommandOutcome::Success {
            value: serde_json::json!({
                "command_ok": false,
                "topic_ok": false,
                "surface_ok": false,
            }),
            warnings: Vec::new(),
        }
    );
    assert!(catalog.describe("late.extra").is_none());
    assert!(!catalog.topics().contains("late.extra"));
    assert_eq!(
        catalog
            .surfaces()
            .into_iter()
            .filter(|surface| surface.module == "late.luau")
            .count(),
        1
    );
}

#[test]
fn a_retired_generation_cannot_enqueue_a_session_reorder() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    let module = directory.path().join("reorder.luau");
    fs::write(
        &module,
        r#"
bootty.commands.register({ id = "reorder.defer", title = "Defer" }, function()
    bootty.commands.invoke({ command = "resource.current", arguments = { "session" } })
    bootty.reorder_session("old", nil)
end)
"#,
    )
    .expect("old reorder generation");
    let catalog = Arc::new(ExtensionCatalog::default());
    let (sender, app_receiver) = app_command_channel(4);
    let mut host = ExtensionHost::load(
        directory.path(),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );
    let old = resolved_handler_named(&catalog, "reorder.defer");
    let old_outcome = old.invoke(
        CommandInvocation::new("reorder.defer", Vec::new(), Caller::Socket),
        Instant::now() + EXTENSION_BUDGET,
        CommandCancellation::new(),
    );
    let deadline = Instant::now() + EXTENSION_BUDGET;
    let nested = loop {
        match app_receiver.try_recv() {
            Ok(request) => break request,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "nested request did not arrive");
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("nested request channel failed: {error}"),
        }
    };

    fs::write(
        &module,
        r#"
bootty.commands.register({ id = "reorder.current", title = "Current" }, function()
    bootty.reorder_session("new", nil)
end)
"#,
    )
    .expect("new reorder generation");
    host.refresh(Instant::now() + Duration::from_secs(1));
    nested
        .response
        .send(CommandOutcome::success())
        .expect("complete nested request");
    assert!(matches!(
        old_outcome.recv_timeout(EXTENSION_BUDGET).expect("old outcome"),
        CommandOutcome::Failed { code, .. }
            if matches!(code.as_str(), "extension_failed" | "stale_extension_generation")
    ));
    let reorders = host.take_session_reorders();
    assert_eq!(reorders, Vec::<SessionReorder>::new());

    assert_eq!(
        invoke_named(&catalog, "reorder.current", EXTENSION_BUDGET),
        CommandOutcome::success()
    );
    assert_eq!(
        host.take_session_reorders(),
        [SessionReorder {
            source: "new".to_owned(),
            before: None,
        }]
    );
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

fn invoke_named(catalog: &ExtensionCatalog, command: &str, limit: Duration) -> CommandOutcome {
    let (_, handler) = catalog.command(command).expect("resolve extension command");
    handler
        .invoke(
            CommandInvocation::new(command, Vec::new(), Caller::Socket),
            Instant::now() + limit,
            CommandCancellation::new(),
        )
        .recv_timeout(EXTENSION_BUDGET)
        .expect("extension command outcome")
}

fn resolved_handler_named(catalog: &ExtensionCatalog, command: &str) -> ExtensionInvocationSender {
    catalog
        .command(command)
        .map(|(_, handler)| handler)
        .expect("resolve extension command")
}

fn success_version(version: u64) -> CommandOutcome {
    CommandOutcome::Success {
        value: serde_json::json!({"version": version}),
        warnings: Vec::new(),
    }
}
