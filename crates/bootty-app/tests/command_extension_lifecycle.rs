use std::{
    fs,
    sync::Arc,
    time::{Duration, Instant},
};

use bootty_app::ui::{
    chrome::{SidebarEvent, SidebarModel, show_sidebar},
    sidebar::build_sidebar_items_from_published_items,
};
use bootty_command::{
    AppCommandReceiver, AppCommandSender, Caller, app_command_channel as command_channel,
};
use bootty_extension::{ExtensionHost, ExtensionUiAction, SurfacePlacement, event_queue};
use bootty_mux::controller::{BindingId, MuxScope, SpaceId};
use egui::{Event, PointerButton, Pos2, RawInput, Rect};

fn app_command_channel(capacity: usize) -> (AppCommandSender, AppCommandReceiver) {
    command_channel(capacity, Arc::new(|| {}))
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
    let catalog = Arc::new(bootty_extension::ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4);
    let mut host = ExtensionHost::load(
        directory.path(),
        catalog.clone(),
        sender.for_caller(Caller::Luau),
        event_queue().0,
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

fn surface_text(
    catalog: &bootty_extension::ExtensionCatalog,
    module: &str,
    surface: &str,
) -> String {
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

fn wait_for_surface_text(catalog: &bootty_extension::ExtensionCatalog, expected: &str) {
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
    let (footer_items, body_items): (Vec<_>, Vec<_>) = host
        .surface(SurfacePlacement::Sidebar, "actions")
        .expect("published sidebar surface")
        .items()
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
                                    title_visible: false,
                                    reserve_titlebar_buttons: false,
                                    title_icon: None,
                                    top_inset: 0.0,
                                    border_visible: false,
                                    border_bottom: false,
                                    separator_visible: false,
                                    focused: false,
                                    hovered_session: None,
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
