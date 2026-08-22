use bootty_app::cli::Cli;
use bootty_font::FontFeature;
use clap::Parser;

#[test]
fn font_feature_cli_overrides_use_the_shared_grammar() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let config_path = directory.path().join("config.toml");
    std::fs::write(&config_path, "[font]\nfeatures = [\"-liga\"]\n").expect("write config");
    let cli = Cli::try_parse_from([
        "bootty",
        "--config",
        config_path.to_str().expect("UTF-8 config path"),
        "--font-feature",
        "cv01=2",
    ])
    .expect("parse CLI");

    let config = cli.load_config().expect("apply valid font override");

    assert_eq!(
        config.font.features,
        vec![
            FontFeature::new(*b"liga", 1),
            FontFeature::new(*b"liga", 0),
            FontFeature::new(*b"cv01", 2),
        ]
    );
}

#[test]
fn invalid_font_feature_cli_override_keeps_the_exact_error() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let config_path = directory.path().join("config.toml");
    std::fs::write(&config_path, "").expect("write config");
    let cli = Cli::try_parse_from([
        "bootty",
        "--config",
        config_path.to_str().expect("UTF-8 config path"),
        "--font-feature",
        "toolong",
    ])
    .expect("parse CLI");

    let error = cli.load_config().expect_err("invalid override must fail");

    assert_eq!(error.to_string(), "invalid font feature: toolong");
}
