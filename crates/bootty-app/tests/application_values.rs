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
    controller::SpaceId,
    snapshot::{MuxPaneAnchor, MuxSession, MuxSessionTag},
};
use egui::{Color32, Key, Modifiers};
use pretty_assertions::{assert_eq, assert_ne};
use proptest::prelude::*;
use rstest::rstest;

#[cfg(windows)]
use bootty_app::strings::home_dir;

#[path = "support/events.rs"]
mod events;

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
        tag: MuxSessionTag::default(),
        windows: Vec::new(),
    }
}

fn opaque(r: u8, g: u8, b: u8) -> Color {
    Color { r, g, b, a: 0xff }
}

#[derive(Debug, proptest_derive::Arbitrary)]
struct SessionNameInput {
    #[proptest(regex = "[a-z]{1,8}(/[a-z]{1,8})?")]
    base: String,
    #[proptest(strategy = "0usize..32")]
    collision_count: usize,
}

#[derive(Debug, proptest_derive::Arbitrary)]
struct SessionNameRecognitionInput {
    #[proptest(regex = "[a-z]{1,8}(/[a-z]{1,8})?")]
    base: String,
    suffix: u16,
    #[proptest(regex = "[a-z]{1,8}")]
    invalid_suffix: String,
}

#[rstest]
#[case::exact("prefix ", "abcd", 4, "prefix abcd")]
#[case::truncated("", "abcde", 4, "abc…")]
#[case::zero_width("prefix", "abc", 0, "prefix")]
fn truncated_labels_append_the_value_within_the_width(
    #[case] prefix: &str,
    #[case] value: &str,
    #[case] width: usize,
    #[case] expected: &str,
) {
    let mut output = prefix.to_owned();
    push_truncated_label(&mut output, value, width);
    assert_eq!(output, expected);
}

proptest! {
    /// Property: CSV escaping is identity for safe fields and otherwise quotes exactly once while
    /// doubling every embedded quote.
    #[test]
    fn csv_fields_escape_only_csv_metacharacters(value in "(?s).{0,64}") {
        let expected = if value.contains([',', '"', '\n', '\r']) {
            format!("\"{}\"", value.replace('"', "\"\""))
        } else {
            value.clone()
        };
        prop_assert_eq!(csv_field(&value), expected);
    }

    /// Property: contiguous collisions choose the first available numeric suffix on the same leaf.
    #[test]
    fn generated_session_name_chooses_the_first_available_suffix(input in any::<SessionNameInput>()) {
        let SessionNameInput { base, collision_count } = input;
        let existing = (0..collision_count)
            .map(|index| if index == 0 { base.clone() } else { format!("{base}-{}", index + 1) })
            .collect::<Vec<_>>();
        let expected = if collision_count == 0 {
            base.clone()
        } else {
            format!("{base}-{}", collision_count + 1)
        };

        prop_assert_eq!(unique_session_name(&base, existing.iter().map(String::as_str)), expected);
    }

    /// Property: only the base or a numeric suffix on the same complete leaf is recognized.
    #[test]
    fn generated_session_name_recognition_matches_the_leaf_grammar(
        input in any::<SessionNameRecognitionInput>(),
    ) {
        let SessionNameRecognitionInput { base, suffix, invalid_suffix } = input;
        let valid_suffix = format!("{base}-{suffix}");
        let invalid_suffix = format!("{base}-{invalid_suffix}");
        let other_path = format!("elsewhere/{base}-2");
        let suffixed_base = format!("{base}-2");
        let different_leaf = format!("{base}line-2");
        prop_assert!(is_uniquified_session_name(&base, &base));
        prop_assert!(is_uniquified_session_name(&valid_suffix, &base));
        prop_assert!(!is_uniquified_session_name(&invalid_suffix, &base));
        prop_assert!(!is_uniquified_session_name(&other_path, &base));
        prop_assert!(!is_uniquified_session_name(&base, &suffixed_base));
        prop_assert!(!is_uniquified_session_name(&different_leaf, &base));
    }
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

#[rstest]
#[case(InputFocus::Sidebar, 0, 4)]
#[case(InputFocus::Terminal, 4, 0)]
fn focus_routes_keyboard_input_to_one_owner(
    #[case] focus: InputFocus,
    #[case] terminal_events: usize,
    #[case] ui_events: usize,
) {
    let routed = route_events(
        focus,
        vec![
            events::key_event(Key::J, Modifiers::NONE),
            events::key_event(Key::K, Modifiers::NONE),
            events::key_event(Key::ArrowDown, Modifiers::NONE),
            events::key_event(Key::ArrowUp, Modifiers::NONE),
        ],
    );

    assert_eq!(routed.terminal_events.len(), terminal_events);
    assert_eq!(routed.ui_events.len(), ui_events);
}

#[test]
fn configured_terminal_colors_drive_the_ui_palette() {
    let mut config = BoottyConfig::default();
    let colors = &mut config.appearance.dark.colors;
    colors.background = Some(opaque(1, 2, 3));
    colors.foreground = Some(opaque(240, 241, 242));
    colors.palette = vec![
        opaque(0, 0, 0),
        opaque(100, 0, 0),
        opaque(0, 100, 0),
        opaque(100, 80, 0),
        opaque(0, 0, 100),
        opaque(80, 0, 100),
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
        scope: SpaceId::from_persistence(10),
        label: "Local".to_owned(),
        sessions: vec![session("$1", "work")],
        selected_session: Some("$1".to_owned()),
        active: true,
        can_return_to_last_session: false,
        display_names: HashMap::new(),
    };
    let remote = BindingSessionGroup {
        scope: SpaceId::from_persistence(20),
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
