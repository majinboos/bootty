use std::collections::HashMap;

use bootty_app::{
    mux::{
        controller::{BindingId, MuxScope, SpaceId},
        snapshot::{MuxPaneAnchor, MuxSession},
    },
    ui::{
        session_navigation::BindingSessionGroup,
        sidebar::{
            SidebarDisplay, SidebarItemKind, SidebarTree, build_binding_sidebar_items,
            build_sidebar_items_from_published_items, session_group, session_suffix,
            sidebar_session_colors,
        },
    },
};
use bootty_extension::{
    ModuleColor, ModuleCoord, ModuleItem, ModulePrimitive, PublishedSurfaceItem,
};
use egui::Color32;

fn scope(binding: i64) -> MuxScope {
    MuxScope::new(
        SpaceId::from_persistence(1),
        BindingId::from_persistence(binding),
    )
}

fn session(id: &str, name: &str, process: &str) -> MuxSession {
    MuxSession {
        id: id.to_owned(),
        name: name.to_owned(),
        active: false,
        anchor: MuxPaneAnchor {
            session_id: id.to_owned(),
            process: Some(process.to_owned()),
            ..MuxPaneAnchor::default()
        },
        active_window_id: None,
        windows: Vec::new(),
    }
}

#[test]
fn extension_session_rows_keep_identity_style_and_selection() {
    let primitive = ModulePrimitive::Text {
        text: "right".to_owned(),
        color: Some(ModuleColor::rgb(0xa6, 0xe3, 0xa1)),
        x: ModuleCoord {
            frac: 1.0,
            px: -8.0,
        },
        y: ModuleCoord { frac: 0.5, px: 0.0 },
        size: 11.0,
        align: "right_center".to_owned(),
        min_width: None,
    };
    let items = vec![
        ModuleItem {
            kind: Some("session".to_owned()),
            text: "api".to_owned(),
            number: Some(1),
            session_id: Some("$1".to_owned()),
            reorder_anchor: Some("work/api".to_owned()),
            fg: Some(ModuleColor::rgb(0x89, 0xb4, 0xfa)),
            dim_fg: Some(ModuleColor::rgb(0x45, 0x5a, 0x7d)),
            current: Some(true),
            active: Some(true),
            primitives: vec![primitive.clone()],
            ..ModuleItem::default()
        },
        ModuleItem {
            kind: Some("footer".to_owned()),
            text: "codex".to_owned(),
            primitives: vec![primitive],
            ..ModuleItem::default()
        },
    ];

    let published = items
        .into_iter()
        .map(|item| PublishedSurfaceItem {
            module: "test.luau".to_owned(),
            generation: 1,
            surface: "sidebar".to_owned(),
            item,
        })
        .collect::<Vec<_>>();
    let rows = build_sidebar_items_from_published_items(&published, scope(0), Some("$1"), true);

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].session_id, Some("$1"));
    assert_eq!(
        rows[0].display,
        SidebarDisplay::Numbered {
            number: 1,
            label: "api"
        }
    );
    assert!(rows[0].current);
    assert!(rows[0].can_return_to_last_session);
    assert!(matches!(
        rows[0].kind,
        SidebarItemKind::Session { active: true }
    ));
    assert_eq!(
        rows[0].color,
        Color32::from_rgba_unmultiplied(0x89, 0xb4, 0xfa, 0xff)
    );
    assert_eq!(
        rows[0].dim_color,
        Color32::from_rgba_unmultiplied(0x45, 0x5a, 0x7d, 0xff)
    );
    assert_eq!(rows[0].primitives.len(), 1);
}

#[test]
fn binding_groups_keep_colliding_backend_ids_scoped() {
    let local_scope = scope(10);
    let remote_scope = scope(20);
    let groups = vec![
        BindingSessionGroup {
            scope: local_scope,
            label: "Local".to_owned(),
            sessions: vec![session("$1", "work", "zsh")],
            selected_session: Some("$1".to_owned()),
            active: true,
            can_return_to_last_session: false,
            display_names: HashMap::new(),
        },
        BindingSessionGroup {
            scope: remote_scope,
            label: "Remote".to_owned(),
            sessions: vec![session("$1", "work", "ssh")],
            selected_session: Some("$1".to_owned()),
            active: false,
            can_return_to_last_session: true,
            display_names: HashMap::new(),
        },
    ];

    let items = build_binding_sidebar_items(&groups);

    assert_eq!(items.len(), 4);
    assert_eq!(items[0].display, SidebarDisplay::Text("Local"));
    assert_eq!(items[2].display, SidebarDisplay::Text("Remote"));
    assert_eq!(items[1].session_scope, Some(local_scope));
    assert_eq!(items[3].session_scope, Some(remote_scope));
    assert!(items[1].current);
    assert!(!items[3].current);
    assert_ne!(items[1].id, items[3].id);
}

#[test]
fn native_sessions_project_to_grouped_sidebar_rows() {
    let sessions = vec![
        session("$1", "work/api", "zsh"),
        session("$2", "work/ui", "nvim"),
    ];
    let groups = [BindingSessionGroup {
        scope: scope(0),
        label: "Native".to_owned(),
        sessions,
        selected_session: Some("$1".to_owned()),
        active: true,
        can_return_to_last_session: false,
        display_names: HashMap::new(),
    }];
    let items = build_binding_sidebar_items(&groups);

    assert_eq!(items.len(), 4);
    assert_eq!(
        items[2].display,
        SidebarDisplay::Numbered {
            number: 1,
            label: "api"
        }
    );
    assert_eq!(items[2].tree, SidebarTree::Middle);
    assert_eq!(items[3].tree, SidebarTree::Last);
}

#[test]
fn selected_session_is_the_only_current_sidebar_row() {
    let mut sessions = vec![session("$1", "one", "zsh"), session("$2", "two", "fish")];
    sessions[0].active = true;
    let groups = [BindingSessionGroup {
        scope: scope(0),
        label: "Native".to_owned(),
        sessions,
        selected_session: Some("$2".to_owned()),
        active: true,
        can_return_to_last_session: false,
        display_names: HashMap::new(),
    }];
    let items = build_binding_sidebar_items(&groups);
    let current = items
        .iter()
        .filter(|item| matches!(item.kind, SidebarItemKind::Session { .. }) && item.current)
        .map(|item| item.session_id)
        .collect::<Vec<_>>();

    assert_eq!(current, vec![Some("$2")]);
}

#[test]
fn ungrouped_sessions_receive_distinct_accent_colors() {
    let sessions = vec![
        session("local", "local", "zsh"),
        session("project", "project", "fish"),
    ];

    let colors = sidebar_session_colors(&sessions, &[]);

    assert_eq!(colors.len(), 2);
    assert_ne!(colors[0].color, colors[1].color);
    assert_ne!(colors[0].dim_color, colors[1].dim_color);
}

#[test]
fn session_grouping_splits_only_at_the_first_slash() {
    assert_eq!(session_group("a/b/c"), "a");
    assert_eq!(session_suffix("a/b/c"), "b/c");
}
