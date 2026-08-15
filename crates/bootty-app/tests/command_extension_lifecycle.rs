use std::{
    fs,
    sync::Arc,
    time::{Duration, Instant},
};

use bootty_app::{
    command_extensions::{ExtensionHost, ExtensionUiAction, SurfacePlacement},
    commands::{
        Caller, CommandCancellation, CommandCatalog, CommandExecutor, CommandInvocation,
        CommandOutcome, app_command_channel_with_repaint,
    },
    control::ControlPlane,
    extension_ui::MuxView,
    mux::controller::{BindingId, MuxScope, SpaceId},
    ui::{
        chrome::{SidebarEvent, SidebarModel, show_sidebar},
        sidebar::build_sidebar_items_from_published_items,
    },
};
use egui::{Event, PointerButton, Pos2, RawInput, Rect};

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

fn app_command_channel(
    capacity: usize,
) -> (
    bootty_app::commands::AppCommandSender,
    bootty_app::commands::AppCommandReceiver,
) {
    app_command_channel_with_repaint(capacity, Arc::new(|| {}))
}

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
    while true do end
end)
"#;

const VERSION_THREE: &str = r#"
bootty.events.register("probe.changed")
bootty.commands.register({ id = "probe.echo", title = "Echo" }, function()
    return { version = 3 }
end)
"#;

#[test]
fn local_module_generation_replaces_atomically_and_retires_runaway_handlers() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    let module = directory.path().join("probe.luau");
    fs::write(&module, VERSION_ONE).expect("write first module generation");
    let catalog = Arc::new(CommandCatalog::default());
    let (sender, _receiver) = app_command_channel(4);
    let mut host = ExtensionHost::load(
        directory.path(),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        ControlPlane::default(),
    );
    let started = Instant::now();

    assert_eq!(invoke(&catalog, Duration::from_secs(1)), success_version(1));
    assert!(catalog.extension_topics().contains("probe.changed"));
    assert_eq!(
        surface_text(&catalog, "probe.luau", "probe.status"),
        "version 1"
    );
    let first = resolved_handler(&catalog);

    fs::write(&module, VERSION_TWO).expect("write second module generation");
    host.refresh(started + Duration::from_secs(1));
    assert_eq!(invoke(&catalog, Duration::from_secs(1)), success_version(2));
    assert_eq!(
        surface_text(&catalog, "probe.luau", "probe.status"),
        "version 2"
    );
    assert!(matches!(
        invoke_handler(first, Duration::from_secs(1)),
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
    assert_eq!(invoke(&catalog, Duration::from_secs(1)), success_version(2));
    assert!(catalog.describe("probe.partial").is_none());
    assert!(catalog.extension_topics().contains("probe.changed"));
    assert_eq!(
        surface_text(&catalog, "probe.luau", "probe.status"),
        "version 2"
    );

    fs::write(&module, RUNAWAY).expect("write runaway module generation");
    host.refresh(started + Duration::from_secs(3));
    let runaway = resolved_handler(&catalog);
    let outcome = runaway(
        CommandInvocation::new("probe.echo", Vec::new(), Caller::Socket),
        Instant::now() + Duration::from_secs(5),
        CommandCancellation::new(),
    );
    fs::write(&module, VERSION_THREE).expect("write replacement module generation");
    let refresh_started = Instant::now();
    host.refresh(started + Duration::from_secs(4));
    assert!(refresh_started.elapsed() < Duration::from_millis(300));
    assert!(matches!(
        outcome.recv_timeout(Duration::from_secs(1)).expect("retired outcome"),
        CommandOutcome::Failed { code, .. } if code == "stale_extension_generation"
    ));
    assert_eq!(invoke(&catalog, Duration::from_secs(1)), success_version(3));

    let third = resolved_handler(&catalog);
    fs::remove_file(&module).expect("remove extension module");
    host.refresh(started + Duration::from_secs(5));
    assert!(catalog.describe("probe.echo").is_none());
    assert!(!catalog.extension_topics().contains("probe.changed"));
    assert!(matches!(
        invoke_handler(third, Duration::from_secs(1)),
        CommandOutcome::Failed { code, .. } if code == "stale_extension_generation"
    ));
}

#[test]
fn runaway_handler_obeys_the_invocation_deadline() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    fs::write(directory.path().join("probe.luau"), RUNAWAY)
        .expect("write runaway module generation");
    let catalog = Arc::new(CommandCatalog::default());
    let (sender, _receiver) = app_command_channel(4);
    let _host = ExtensionHost::load(
        directory.path(),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        ControlPlane::default(),
    );

    let started = Instant::now();
    assert!(matches!(
        invoke(&catalog, Duration::from_millis(50)),
        CommandOutcome::Failed { code, .. } if code == "deadline_exceeded"
    ));
    assert!(started.elapsed() < Duration::from_millis(300));
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
    let catalog = Arc::new(CommandCatalog::default());
    let (sender, _receiver) = app_command_channel(4);
    {
        let _host = ExtensionHost::load(
            &extension_root,
            Arc::clone(&catalog),
            sender.for_caller(Caller::Luau),
            ControlPlane::default(),
        );
        assert_eq!(
            invoke_named(&catalog, "counter.increment", Duration::from_secs(1)),
            CommandOutcome::Success {
                value: serde_json::json!({"count": 1}),
                warnings: Vec::new(),
            }
        );
        assert!(matches!(
            invoke_named(&catalog, "counter.oversized", Duration::from_secs(1)),
            CommandOutcome::Failed { code, message }
                if code == "extension_failed"
                    && message.contains("extension storage value exceeds 65536 bytes")
        ));
    }

    let mut host = ExtensionHost::load(
        &extension_root,
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        ControlPlane::default(),
    );
    assert_eq!(
        invoke_named(&catalog, "counter.increment", Duration::from_secs(1)),
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
            invoke_named(&catalog, "counter.handoff", Duration::from_secs(1)),
            CommandOutcome::success()
        );
        refresh.join().expect("publish handoff generation");
    });
    assert_eq!(
        invoke_named(&catalog, "counter.increment", Duration::from_secs(1)),
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
    let catalog = Arc::new(CommandCatalog::default());
    let (sender, _receiver) = app_command_channel(4);
    let host = ExtensionHost::load(
        directory.path(),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        ControlPlane::default(),
    );

    let initial = catalog
        .extension_surfaces()
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
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let refreshed = catalog
            .extension_surfaces()
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
    let catalog = Arc::new(CommandCatalog::default());
    let (sender, _receiver) = app_command_channel(4);
    let mut host = ExtensionHost::load(
        directory.path(),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        ControlPlane::default(),
    );
    let started = Instant::now();
    let first = host
        .surfaces(bootty_app::command_extensions::SurfacePlacement::Status)
        .into_iter()
        .find(|surface| surface.module == "mixed.luau")
        .expect("first status surface");
    assert_eq!(
        invoke_named(&catalog, "mixed.version", Duration::from_secs(1)),
        success_version(1)
    );
    assert!(catalog.extension_topics().contains("mixed.changed"));

    fs::write(&module, MIXED_GENERATION.replace("__VERSION__", "2"))
        .expect("second mixed generation");
    host.refresh(started + Duration::from_secs(1));
    assert_eq!(
        invoke_named(&catalog, "mixed.version", Duration::from_secs(1)),
        success_version(2)
    );
    let second = catalog
        .extension_surfaces()
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
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let current = catalog
            .extension_surfaces()
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
        invoke_named(&catalog, "mixed.version", Duration::from_secs(1)),
        success_version(2)
    );
    assert!(catalog.extension_topics().contains("mixed.changed"));
    assert!(catalog.extension_surfaces().into_iter().all(|surface| {
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
    let catalog = Arc::new(CommandCatalog::default());
    let (sender, _receiver) = app_command_channel(4);
    let mut host = ExtensionHost::load(
        directory.path(),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        ControlPlane::default(),
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
    let deadline = Instant::now() + Duration::from_secs(1);
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
    let catalog = Arc::new(CommandCatalog::default());
    let (sender, _receiver) = app_command_channel(4);
    let _host = ExtensionHost::load(
        directory.path(),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        ControlPlane::default(),
    );

    assert_eq!(surface_text(&catalog, "a.luau", "shared"), "first");
    assert!(catalog.describe("b.command").is_none());
    assert!(!catalog.extension_topics().contains("b.changed"));
    assert!(
        catalog
            .extension_surfaces()
            .iter()
            .all(|surface| surface.module != "b.luau")
    );
}

#[test]
fn sidebar_body_and_footer_actions_keep_the_exact_generation() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    let module = directory.path().join("sidebar-actions.luau");
    let source = |version| {
        format!(
            r#"
local version = {version}
local selected = "initial-" .. version
bootty.ui.register({{ id = "actions", placement = "sidebar" }}, function()
    return {{
        {{ text = "body:" .. selected, key = "body", action = "body" }},
        {{ text = "footer:" .. selected, kind = "footer", key = "footer", action = "footer" }},
    }}
end, function(action)
    selected = action .. "-" .. version
end)
"#
        )
    };
    fs::write(&module, source(1)).expect("first sidebar generation");
    let catalog = Arc::new(CommandCatalog::default());
    let (sender, _receiver) = app_command_channel(4);
    let mut host = ExtensionHost::load(
        directory.path(),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        ControlPlane::default(),
    );

    let old_body = click_sidebar_action(&host, false);
    assert_eq!(old_body.module, "sidebar-actions.luau");
    assert_eq!(old_body.surface, "actions");
    assert_eq!(old_body.action, "body");
    assert_eq!(old_body.payload, serde_json::Value::Null);
    host.submit_ui_action(old_body.clone())
        .expect("current body action");
    wait_for_surface_text(&catalog, "body:body-1");

    let current_footer = click_sidebar_action(&host, true);
    assert_eq!(current_footer.action, "footer");
    assert_eq!(current_footer.generation, old_body.generation);
    host.submit_ui_action(current_footer)
        .expect("current footer action");
    wait_for_surface_text(&catalog, "body:footer-1");

    fs::write(&module, source(2)).expect("second sidebar generation");
    host.refresh(Instant::now() + Duration::from_secs(1));
    assert_eq!(
        host.submit_ui_action(old_body.clone()),
        Err("extension generation is no longer active".to_owned())
    );
    let current_body = click_sidebar_action(&host, false);
    assert_ne!(current_body.generation, old_body.generation);
    host.submit_ui_action(current_body)
        .expect("replacement body action");
    wait_for_surface_text(&catalog, "body:body-2");
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
    let catalog = Arc::new(CommandCatalog::default());
    let (sender, _receiver) = app_command_channel(4);
    let _host = ExtensionHost::load(
        directory.path(),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        ControlPlane::default(),
    );

    assert_eq!(
        invoke_named(&catalog, "late.try", Duration::from_secs(1)),
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
    assert!(!catalog.extension_topics().contains("late.extra"));
    assert_eq!(
        catalog
            .extension_surfaces()
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
    let catalog = Arc::new(CommandCatalog::default());
    let (sender, app_receiver) = app_command_channel(4);
    let mut host = ExtensionHost::load(
        directory.path(),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        ControlPlane::default(),
    );
    let old = resolved_handler_named(&catalog, "reorder.defer");
    let old_outcome = old(
        CommandInvocation::new("reorder.defer", Vec::new(), Caller::Socket),
        Instant::now() + Duration::from_secs(2),
        CommandCancellation::new(),
    );
    let deadline = Instant::now() + Duration::from_secs(1);
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
        old_outcome.recv_timeout(Duration::from_secs(1)).expect("old outcome"),
        CommandOutcome::Failed { code, .. }
            if matches!(code.as_str(), "extension_failed" | "stale_extension_generation")
    ));
    assert!(host.take_session_reorders().is_empty());

    assert_eq!(
        invoke_named(&catalog, "reorder.current", Duration::from_secs(1)),
        CommandOutcome::success()
    );
    assert_eq!(
        host.take_session_reorders(),
        [bootty_app::command_extensions::SessionReorder {
            source: "new".to_owned(),
            before: None,
        }]
    );
}

fn surface_text(catalog: &CommandCatalog, module: &str, surface: &str) -> String {
    catalog
        .extension_surfaces()
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

fn wait_for_surface_text(catalog: &CommandCatalog, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if surface_text(catalog, "sidebar-actions.luau", "actions") == expected {
            return;
        }
        assert!(Instant::now() < deadline, "sidebar action did not publish");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn click_sidebar_action(host: &ExtensionHost, footer: bool) -> ExtensionUiAction {
    let published = host
        .surface(SurfacePlacement::Sidebar, "actions")
        .expect("published sidebar surface")
        .into_items();
    let (footer_items, body_items): (Vec<_>, Vec<_>) = published
        .into_iter()
        .partition(|item| item.item.kind.as_deref() == Some("footer"));
    let scope = MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(1));
    let rows = build_sidebar_items_from_published_items(&body_items, scope, None, false);
    let context = egui::Context::default();
    let screen = Rect::from_min_size(Pos2::ZERO, egui::vec2(240.0, 160.0));
    let point = if footer {
        Pos2::new(80.0, 137.0)
    } else {
        Pos2::new(80.0, 12.0)
    };
    let show = |events| {
        let mut event = None;
        context
            .run_ui(
                RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..RawInput::default()
                },
                |ui| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show(ui, |ui| {
                            event = show_sidebar(
                                ui,
                                bootty_ui::ThemePalette::default(),
                                screen.height(),
                                SidebarModel {
                                    items: &rows,
                                    footer_items: &footer_items,
                                    session_count: 1,
                                    has_sessions: true,
                                    title_visible: false,
                                    reserve_titlebar_buttons: false,
                                    title_icon: None,
                                    top_inset: 0.0,
                                    border_visible: false,
                                    border_bottom: false,
                                    separator_visible: false,
                                    focused: false,
                                    hovered_session: None,
                                    unfocused_dim: 0.0,
                                    fullscreen: false,
                                    hover_override: None,
                                    current_override: None,
                                    border_override: None,
                                },
                            );
                        });
                },
            )
            .drop_without_applying_deltas();
        event
    };

    show(vec![Event::PointerMoved(point)]);
    show(vec![Event::PointerButton {
        pos: point,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    }]);
    match show(vec![Event::PointerButton {
        pos: point,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    }]) {
        Some(SidebarEvent::ExtensionAction(action)) => action,
        other => panic!("expected sidebar extension action, got {other:?}"),
    }
}

fn invoke(catalog: &CommandCatalog, limit: Duration) -> CommandOutcome {
    invoke_named(catalog, "probe.echo", limit)
}

fn invoke_named(catalog: &CommandCatalog, command: &str, limit: Duration) -> CommandOutcome {
    let resolved = catalog
        .resolve(CommandInvocation::new(command, Vec::new(), Caller::Socket))
        .expect("resolve extension command");
    let CommandExecutor::Extension(handler) = resolved.executor else {
        panic!("extension command handler");
    };
    handler(
        resolved.invocation,
        Instant::now() + limit,
        CommandCancellation::new(),
    )
    .recv_timeout(Duration::from_secs(1))
    .expect("extension command outcome")
}

fn resolved_handler(catalog: &CommandCatalog) -> bootty_app::commands::ExtensionCommandHandler {
    resolved_handler_named(catalog, "probe.echo")
}

fn resolved_handler_named(
    catalog: &CommandCatalog,
    command: &str,
) -> bootty_app::commands::ExtensionCommandHandler {
    let resolved = catalog
        .resolve(CommandInvocation::new(command, Vec::new(), Caller::Socket))
        .expect("resolve extension command");
    let CommandExecutor::Extension(handler) = resolved.executor else {
        panic!("extension command handler");
    };
    handler
}

fn invoke_handler(
    handler: bootty_app::commands::ExtensionCommandHandler,
    limit: Duration,
) -> CommandOutcome {
    handler(
        CommandInvocation::new("probe.echo", Vec::new(), Caller::Socket),
        Instant::now() + limit,
        CommandCancellation::new(),
    )
    .recv_timeout(Duration::from_secs(1))
    .expect("extension command outcome")
}

fn success_version(version: u64) -> CommandOutcome {
    CommandOutcome::Success {
        value: serde_json::json!({"version": version}),
        warnings: Vec::new(),
    }
}
