use std::collections::HashMap;

use bootty_ui::push_truncated_label;

use bootty_app::{
    input::{focus::InputFocus, router::route_events},
    strings::{csv_field, is_uniquified_session_name, unique_session_name},
    theme::theme_palette_from_colors,
    ui::session_navigation::BindingSessionGroup,
};
use bootty_config::{color::Color, config::BoottyConfig};
use bootty_mux::{
    controller::{BindingId, MuxScope, SpaceId},
    snapshot::{MuxPaneAnchor, MuxSession},
};
use egui::{Color32, Event, Key, Modifiers};

#[cfg(windows)]
use bootty_app::strings::home_dir;

fn key_event(key: Key) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }
}

fn scope(binding_id: i64) -> MuxScope {
    MuxScope::new(
        SpaceId::from_persistence(1),
        BindingId::from_persistence(binding_id),
    )
}

fn session(id: &str, name: &str) -> MuxSession {
    MuxSession {
        id: id.to_owned(),
        name: name.to_owned(),
        active: false,
        anchor: MuxPaneAnchor {
            session_id: id.to_owned(),
            ..Default::default()
        },
        active_window_id: None,
        windows: Vec::new(),
    }
}

#[test]
fn csv_fields_quote_only_values_that_require_it() {
    assert_eq!(csv_field("arc/dblclick"), "arc/dblclick");
    assert_eq!(csv_field("a,b"), "\"a,b\"");
    assert_eq!(csv_field("a\"b"), "\"a\"\"b\"");
    assert_eq!(csv_field("a\nb"), "\"a\nb\"");
}

#[test]
fn truncated_labels_append_exact_truncated_and_zero_width_values() {
    let mut exact = String::from("prefix ");
    push_truncated_label(&mut exact, "abcd", 4);
    assert_eq!(exact, "prefix abcd");

    let mut truncated = String::new();
    push_truncated_label(&mut truncated, "abcde", 4);
    assert_eq!(truncated, "abc…");

    let mut zero = String::from("prefix");
    push_truncated_label(&mut zero, "abc", 0);
    assert_eq!(zero, "prefix");
}

#[test]
fn session_names_suffix_only_real_collisions() {
    assert_eq!(
        unique_session_name("bootty/review", ["bootty/review", "bootty/review-2"]),
        "bootty/review-3"
    );
    assert_eq!(unique_session_name("scratch", ["scratch"]), "scratch-2");
    assert_eq!(
        unique_session_name("bootty/main", ["other/main"]),
        "bootty/main"
    );
}

#[test]
fn generated_session_names_require_a_numeric_suffix_on_the_same_leaf() {
    assert!(is_uniquified_session_name("bootty/main", "bootty/main"));
    assert!(is_uniquified_session_name("bootty/main-2", "bootty/main"));
    assert!(is_uniquified_session_name("bootty/main-13", "bootty/main"));
    assert!(is_uniquified_session_name("scratch-2", "scratch"));

    assert!(!is_uniquified_session_name("bootty/release", "bootty/main"));
    assert!(!is_uniquified_session_name(
        "bootty/main-next",
        "bootty/main"
    ));
    assert!(!is_uniquified_session_name("bootty/main-", "bootty/main"));
    assert!(!is_uniquified_session_name("other/main-2", "bootty/main"));
    assert!(!is_uniquified_session_name("bootty/main", "bootty/main-2"));
    assert!(!is_uniquified_session_name(
        "bootty/mainline-2",
        "bootty/main"
    ));
}

#[cfg(windows)]
#[test]
fn home_expansion_accepts_the_windows_separator() {
    let Some(home) = home_dir() else {
        return;
    };

    assert_eq!(
        bootty_app::strings::expand_home_path(r"~\src"),
        home.join("src")
    );
}

#[test]
fn sidebar_focus_routes_navigation_away_from_the_terminal() {
    let routed = route_events(
        InputFocus::Sidebar,
        vec![
            key_event(Key::J),
            key_event(Key::K),
            key_event(Key::ArrowDown),
            key_event(Key::ArrowUp),
        ],
    );

    assert!(routed.terminal_events.is_empty());
    assert_eq!(routed.ui_events.len(), 4);
}

#[test]
fn terminal_focus_routes_input_to_the_terminal() {
    let routed = route_events(InputFocus::Terminal, vec![key_event(Key::J)]);

    assert_eq!(routed.terminal_events.len(), 1);
    assert!(routed.ui_events.is_empty());
}

#[test]
fn configured_terminal_colors_drive_the_ui_palette() {
    let mut config = BoottyConfig::default();
    let colors = &mut config.appearance.dark.colors;
    colors.background = Some(Color {
        r: 1,
        g: 2,
        b: 3,
        a: 0xff,
    });
    colors.foreground = Some(Color {
        r: 240,
        g: 241,
        b: 242,
        a: 0xff,
    });
    colors.palette = vec![
        Color {
            r: 0,
            g: 0,
            b: 0,
            a: 0xff,
        },
        Color {
            r: 100,
            g: 0,
            b: 0,
            a: 0xff,
        },
        Color {
            r: 0,
            g: 100,
            b: 0,
            a: 0xff,
        },
        Color {
            r: 100,
            g: 80,
            b: 0,
            a: 0xff,
        },
        Color {
            r: 0,
            g: 0,
            b: 100,
            a: 0xff,
        },
        Color {
            r: 80,
            g: 0,
            b: 100,
            a: 0xff,
        },
    ];

    let palette = theme_palette_from_colors(colors);

    assert_eq!(palette.base, Color32::from_rgb(1, 2, 3));
    assert_eq!(palette.text, Color32::from_rgb(240, 241, 242));
    assert_eq!(palette.primary, Color32::from_rgb(80, 0, 100));
    assert_eq!(palette.accent, Color32::from_rgb(0, 0, 100));
    assert_eq!(palette.warning, Color32::from_rgb(100, 80, 0));
    assert_eq!(palette.success, Color32::from_rgb(0, 100, 0));
}

#[test]
fn colliding_backend_session_ids_remain_scoped_navigation_targets() {
    let local = BindingSessionGroup {
        scope: scope(10),
        label: "Local".to_owned(),
        sessions: vec![session("$1", "work")],
        selected_session: Some("$1".to_owned()),
        active: true,
        can_return_to_last_session: false,
        display_names: HashMap::new(),
    };
    let remote = BindingSessionGroup {
        scope: scope(20),
        label: "Remote".to_owned(),
        sessions: vec![session("$1", "work")],
        selected_session: Some("$1".to_owned()),
        active: false,
        can_return_to_last_session: false,
        display_names: HashMap::new(),
    };

    assert_ne!(
        local.target(&local.sessions[0]),
        remote.target(&remote.sessions[0])
    );
    assert!(local.session_is_current(&local.sessions[0]));
    assert!(!remote.session_is_current(&remote.sessions[0]));
}
