use std::sync::Arc;

use bootty_app::{
    app::{AppEffect, AppState},
    config::load_config_from_path,
};
use bootty_font::FontFeature;

mod support;

#[test]
fn font_reload_publishes_the_complete_realized_text_config() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let config_path = directory.path().join("config.toml");
    std::fs::write(&config_path, "").expect("write initial config");
    let config = load_config_from_path(&config_path).expect("load initial config");
    let mut state = AppState::new(config, support::backends(), Arc::new(|| {}), None, None)
        .expect("start app state");

    std::fs::write(
        &config_path,
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
        text.font_features,
        vec![
            FontFeature::new(*b"liga", 1),
            FontFeature::new(*b"liga", 0),
            FontFeature::new(*b"cv01", 2),
            FontFeature::new(*b"ss05", 1),
        ]
    );
    assert_eq!(text.families, ["Test Mono", "monospace"]);
    assert_eq!(text.font_size, 18.0);
    assert_eq!(text.cell_width, Some(10.0));
    assert_eq!(text.cell_height, Some(22.0));
    assert!(!text.fit_cell_height);
    assert!(text.fit_cell_width);
    assert_eq!(text.baseline_adjustment, 4.0);
    assert_eq!(text.underline_position, 3.0);
    assert_eq!(text.underline_thickness, 2.0);
    assert!(text.codepoint_overrides.is_empty());
}

#[test]
fn invalid_font_reload_keeps_the_last_good_font_config() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let config_path = directory.path().join("config.toml");
    std::fs::write(&config_path, "[font]\nfeatures = [\"-liga\"]\n").expect("write valid config");
    let config = load_config_from_path(&config_path).expect("load valid config");
    let mut state = AppState::new(config, support::backends(), Arc::new(|| {}), None, None)
        .expect("start app state");
    let last_good_font = state.config().font.clone();

    std::fs::write(&config_path, "[font]\nfeatures = [\"toolong\"]\n")
        .expect("write invalid config");
    let mut effects = Vec::new();

    assert!(!state.reload_config(&mut effects));
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::SetTerminalTextConfig(_)))
    );
    assert_eq!(state.config().font, last_good_font);
    assert_eq!(state.last_error(), Some("invalid font feature: toolong"));
}
