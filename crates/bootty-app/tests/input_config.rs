use assert_fs::{TempDir, prelude::*};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};

use std::{sync::Arc, time::Instant};

use bootty_app::{AppEffect, AppState, input::resolve_modifier_remaps};
use bootty_config::config::load_config_from_path;

#[path = "support/events.rs"]
mod events;
#[path = "support/frames.rs"]
mod frames;
mod support;

fn entries(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[fixture]
fn config_directory() -> TempDir {
    TempDir::new().expect("temporary config directory")
}

#[test]
fn modifier_remaps_preserve_source_expansion_and_final_order() {
    let remaps = resolve_modifier_remaps(&entries(&["control=option", "right_shift=command"]))
        .expect("valid modifier remaps");

    assert_eq!(
        remaps.formatted_entries(),
        entries(&[
            "right_ctrl=left_alt",
            "right_shift=left_super",
            "left_ctrl=left_alt",
        ])
    );
}

#[test]
fn modifier_remap_errors_preserve_the_source_entry_and_parser_message() {
    let missing_assignment =
        resolve_modifier_remaps(&entries(&["alt"])).expect_err("missing assignment must fail");
    assert_eq!(
        missing_assignment.to_string(),
        "invalid modifier-remap \"alt\": missing modifier remap assignment"
    );

    let invalid_modifier = resolve_modifier_remaps(&entries(&["middle_ctrl=super"]))
        .expect_err("invalid modifier must fail");
    assert_eq!(
        invalid_modifier.to_string(),
        "invalid modifier-remap \"middle_ctrl=super\": invalid modifier remap modifier \"middle_ctrl\""
    );

    let startup_error = anyhow::Error::new(missing_assignment);
    assert_eq!(
        format!("{startup_error:#}"),
        "invalid modifier-remap \"alt\": missing modifier remap assignment"
    );
}

#[test]
fn a_failed_modifier_remap_sequence_publishes_no_partial_set() {
    let error = resolve_modifier_remaps(&entries(&["alt=ctrl", "broken", "shift=cmd"]))
        .expect_err("the first invalid entry must reject the sequence");
    assert_eq!(
        error.to_string(),
        "invalid modifier-remap \"broken\": missing modifier remap assignment"
    );

    let remaps = resolve_modifier_remaps(&entries(&["shift=cmd"]))
        .expect("a later independent realization must start empty");
    assert_eq!(
        remaps.formatted_entries(),
        entries(&["right_shift=left_super", "left_shift=left_super"])
    );
}

#[rstest]
#[case(
    "[input]\nmodifier-remap = [\"alt\"]\n",
    "invalid modifier-remap \"alt\": missing modifier remap assignment"
)]
#[case(
    "[input]\nkeybind = [\"clear\", \"broken\"]\n",
    "invalid keybind \"broken\""
)]
#[case(
    "[multiplexer]\nbackend = \"native\"\n\n[input.backend-keybind]\ntmux = [\"clear\", \"broken\"]\n",
    "invalid keybind \"broken\""
)]
fn invalid_input_config_stops_startup_before_the_workspace_opens(
    config_directory: TempDir,
    #[case] source: &str,
    #[case] expected_error: &str,
) {
    let config_file = config_directory.child("config.toml");
    config_file.write_str(source).expect("write config");
    let config = load_config_from_path(config_file.path()).expect("load structurally valid config");

    let error = match AppState::new(config, support::backends(), Arc::new(|| {}), None, None) {
        Ok(_) => panic!("invalid input must stop startup"),
        Err(error) => error,
    };
    assert!(error.to_string().contains(expected_error));
    assert!(
        !config_directory
            .child("session-order.sqlite3")
            .path()
            .exists()
    );
}

#[rstest]
fn invalid_modifier_remap_reload_keeps_the_last_good_config(config_directory: TempDir) {
    let config_file = config_directory.child("config.toml");
    config_file
        .write_str("[input]\nmodifier-remap = [\"alt=ctrl\"]\n")
        .expect("write valid config");
    let config = load_config_from_path(config_file.path()).expect("load valid config");
    let mut state = AppState::new(config, support::backends(), Arc::new(|| {}), None, None)
        .expect("start app state");

    config_file
        .write_str("[input]\nmodifier-remap = [\"alt\"]\n")
        .expect("write invalid modifier remap");
    assert!(!state.reload_config(&mut Vec::new()));
    assert_eq!(state.config().input.modifier_remap, entries(&["alt=ctrl"]));
    assert_eq!(
        state.last_error().as_deref(),
        Some("invalid modifier-remap \"alt\": missing modifier remap assignment")
    );
}

#[rstest]
fn invalid_keybind_reload_keeps_the_last_good_derived_binding(config_directory: TempDir) {
    let config_file = config_directory.child("config.toml");
    config_file
        .write_str("[input]\nkeybind = [\"clear\", \"ctrl+k=open_settings\"]\n")
        .expect("write valid config");
    let config = load_config_from_path(config_file.path()).expect("load valid config");
    let mut state = AppState::new(config, support::backends(), Arc::new(|| {}), None, None)
        .expect("start app state");

    config_file
        .write_str("[input]\nkeybind = [\"clear\", \"broken\"]\n")
        .expect("write invalid keybind");
    assert!(!state.reload_config(&mut Vec::new()));

    let effects = state.update_frame(frames::frame(
        Instant::now(),
        vec![events::key_event(
            egui::Key::K,
            egui::Modifiers {
                ctrl: true,
                ..egui::Modifiers::NONE
            },
        )],
    ));
    assert!(effects.contains(&AppEffect::OpenSettings));
}
