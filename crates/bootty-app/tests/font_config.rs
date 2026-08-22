use std::sync::Arc;

use assert_fs::{TempDir, fixture::ChildPath, prelude::*};
use bootty_app::{AppEffect, AppState};
use bootty_config::config::load_config_from_path;
use bootty_font::FontFeature;
use bootty_render::terminal_text::{CodepointFontMap, TerminalTextConfig};
use pretty_assertions::assert_eq;

mod support;

fn state_with_config(source: &str) -> (TempDir, ChildPath, AppState) {
    let directory = TempDir::new().expect("temporary config directory");
    let config = directory.child("config.toml");
    config.write_str(source).expect("write initial config");
    let loaded = load_config_from_path(config.path()).expect("load initial config");
    let state = AppState::new(loaded, support::backends(), Arc::new(|| {}), None, None)
        .expect("start app state");
    (directory, config, state)
}

#[test]
fn font_reload_publishes_the_complete_realized_text_config() {
    let (_directory, config, mut state) = state_with_config("");

    config
        .write_str(
            r#"
font-feature = ["ss05"]

[font]
family = ["Test Mono", "monospace"]
features = ["-liga", "cv01=2"]
size = 18
cell-width = 10
cell-height = 22
fit-cell-height = false
fit-cell-width = true
baseline-adjustment = 4
underline-position = 3
underline-thickness = 2
"#,
        )
        .expect("write changed config");
    let mut effects = Vec::new();

    assert!(state.reload_config(&mut effects));
    let text = effects
        .iter()
        .find_map(|effect| match effect {
            AppEffect::SetTerminalTextConfig(config) => Some(config),
            _ => None,
        })
        .expect("font change publishes a terminal text config");
    assert_eq!(
        text,
        &TerminalTextConfig {
            families: vec!["Test Mono".to_owned(), "monospace".to_owned()],
            font_features: vec![
                FontFeature::new(*b"liga", 1),
                FontFeature::new(*b"liga", 0),
                FontFeature::new(*b"cv01", 2),
                FontFeature::new(*b"ss05", 1),
            ],
            codepoint_overrides: CodepointFontMap::default(),
            font_size: 18.0,
            cell_width: Some(10.0),
            cell_height: Some(22.0),
            fit_cell_height: false,
            fit_cell_width: true,
            baseline_adjustment: 4.0,
            underline_position: 3.0,
            underline_thickness: 2.0,
        }
    );
}

#[test]
fn invalid_font_reload_keeps_the_last_good_font_config() {
    let (_directory, config, mut state) = state_with_config("[font]\nfeatures = [\"-liga\"]\n");
    let last_good_font = state.config().font.clone();

    config
        .write_str("[font]\nfeatures = [\"toolong\"]\n")
        .expect("write invalid config");
    let mut effects = Vec::new();

    assert!(!state.reload_config(&mut effects));
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::SetTerminalTextConfig(_)))
    );
    assert_eq!(state.config().font, last_good_font);
    assert_eq!(
        state.last_error().as_deref(),
        Some("invalid font feature: toolong")
    );
}
