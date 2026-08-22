use bootty_app::ui::extension_window::filtered;
use bootty_app::ui::sidebar::{
    build_sidebar_items_from_published_items, session_group, sidebar_session_colors,
};
use bootty_extension::{
    ModuleColor, ModuleCoord, ModuleItem, ModulePrimitive, PublishedSurfaceItem,
};
use bootty_mux::{
    controller::SpaceId,
    snapshot::{MuxPaneAnchor, MuxSession, MuxSessionTag},
};
use egui::Color32;
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

fn extension_item(key: &str, text: &str) -> ModuleItem {
    ModuleItem {
        text: text.to_owned(),
        key: Some(key.to_owned()),
        ..ModuleItem::default()
    }
}

#[rstest]
#[case::text("logs", &[1])]
#[case::key("a", &[0])]
#[case::empty("", &[0, 1])]
#[case::missing("zzz", &[])]
fn extension_filter_matches_item_text_or_key(#[case] query: &str, #[case] expected: &[usize]) {
    let items = vec![
        extension_item("a", "Restart server"),
        extension_item("b", "Open logs"),
    ];
    assert_eq!(filtered(&items, query), expected);
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
        tag: MuxSessionTag::default(),
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
    let rows = build_sidebar_items_from_published_items(
        &published,
        SpaceId::from_persistence(0),
        Some("$1"),
        true,
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].session_id, Some("$1"));
    assert_eq!((rows[0].number, rows[0].text), (Some(1), "api"));
    assert!(rows[0].current);
    assert!(rows[0].can_return_to_last_session);
    assert_eq!(rows[0].context_position, Some((0, 1)));
    assert_eq!(rows[0].kind, "session");
    assert!(rows[0].active);
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

proptest! {
    /// Property: grouping returns exactly the prefix before the first slash.
    #[test]
    fn session_grouping_splits_only_at_the_first_slash(
        segments in proptest::collection::vec("[a-z]{1,12}", 1..8),
    ) {
        let name = segments.join("/");
        prop_assert_eq!(session_group(&name), segments[0].as_str());
    }

    /// Property: different ungrouped sessions receive different bright and dim accents.
    #[test]
    fn different_ungrouped_sessions_receive_distinct_accents(
        (first, second) in ("[a-z]{1,12}", "[a-z]{1,12}")
            .prop_filter("session names differ", |(first, second)| first != second),
    ) {
        let sessions = [session("first", &first, "zsh"), session("second", &second, "fish")];
        let colors = sidebar_session_colors(&sessions, &[] as &[String]);

        prop_assert_ne!(colors[0].0, colors[1].0);
        prop_assert_ne!(colors[0].1, colors[1].1);
    }
}
