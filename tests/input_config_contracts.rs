use std::sync::Arc;

use bootty_app::{app::AppState, config::load_config_from_path, input::resolve_modifier_remaps};

fn entries(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
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

#[test]
fn invalid_modifier_remap_startup_fails_before_the_workspace_opens() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let config_path = directory.path().join("config.toml");
    std::fs::write(&config_path, "[input]\nmodifier-remap = [\"alt\"]\n")
        .expect("write structurally valid config");
    let config = load_config_from_path(&config_path).expect("load structurally valid config");

    let error = match AppState::new(config, Arc::new(|| {}), None, None) {
        Ok(_) => panic!("invalid modifier remap must stop startup"),
        Err(error) => error,
    };
    assert_eq!(
        error.to_string(),
        "invalid modifier-remap \"alt\": missing modifier remap assignment"
    );
    assert!(!directory.path().join("session-order.sqlite3").exists());
}

#[test]
fn invalid_modifier_remap_reload_keeps_the_last_good_config() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let config_path = directory.path().join("config.toml");
    std::fs::write(&config_path, "[input]\nmodifier-remap = [\"alt=ctrl\"]\n")
        .expect("write valid config");
    let config = load_config_from_path(&config_path).expect("load valid config");
    let mut state = AppState::new(config, Arc::new(|| {}), None, None).expect("start app state");

    std::fs::write(&config_path, "[input]\nmodifier-remap = [\"alt\"]\n")
        .expect("write invalid modifier remap");
    assert!(!state.reload_config(&mut Vec::new()));
    assert_eq!(state.config().input.modifier_remap, entries(&["alt=ctrl"]));
    assert_eq!(
        state.last_error(),
        Some("invalid modifier-remap \"alt\": missing modifier remap assignment")
    );
}
