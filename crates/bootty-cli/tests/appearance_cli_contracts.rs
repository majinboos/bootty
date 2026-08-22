use std::ffi::OsString;

use anyhow::Result;
use bootty_cli::Cli;
use bootty_config::{
    color::Color,
    config::{AppearanceMode, AppearanceVariant, BoottyConfig},
};
use clap::Parser;

fn load_with_overrides(source: &str, overrides: &[&str]) -> Result<BoottyConfig> {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let config_path = directory.path().join("config.toml");
    std::fs::write(&config_path, source).expect("write config");
    let mut arguments = vec![
        OsString::from("bootty"),
        OsString::from("app"),
        OsString::from("--config"),
        config_path.into_os_string(),
    ];
    arguments.extend(overrides.iter().map(|argument| OsString::from(*argument)));
    Cli::try_parse_from(arguments)
        .expect("parse appearance overrides")
        .load_config()
}

#[test]
fn appearance_cli_theme_and_colors_seed_both_branches_in_order() {
    let config = load_with_overrides(
        r#"
            [appearance]
            mode = "light"

            [appearance.light]
            theme = "Atom One Light"

            [appearance.dark]
            theme = "Dracula"
        "#,
        &[
            "--theme",
            "Catppuccin Mocha",
            "--background",
            "#101112",
            "--foreground",
            "#202122",
            "--cursor-color",
            "#303132",
            "--cursor-text",
            "#404142",
            "--selection-background",
            "#505152",
            "--selection-foreground",
            "#606162",
            "--palette",
            "#010203,#040506",
            "--palette-generate",
            "--palette-harmonious",
        ],
    )
    .expect("apply appearance overrides");

    assert_eq!(config.appearance.mode, AppearanceMode::Light);
    for variant in [AppearanceVariant::Light, AppearanceVariant::Dark] {
        let colors = config.colors_for_appearance(variant);
        assert_eq!(
            config.theme_for_appearance(variant),
            Some("Catppuccin Mocha")
        );
        assert_eq!(colors.background, Some(Color::from_hex("#101112").unwrap()));
        assert_eq!(colors.foreground, Some(Color::from_hex("#202122").unwrap()));
        assert_eq!(colors.cursor, Some(Color::from_hex("#303132").unwrap()));
        assert_eq!(
            colors.cursor_text,
            Some(Color::from_hex("#404142").unwrap())
        );
        assert_eq!(
            colors.selection_background,
            Some(Color::from_hex("#505152").unwrap())
        );
        assert_eq!(
            colors.selection_foreground,
            Some(Color::from_hex("#606162").unwrap())
        );
        assert_eq!(
            colors.palette,
            [
                Color::from_hex("#010203").unwrap(),
                Color::from_hex("#040506").unwrap(),
            ]
        );
        assert!(colors.palette_generate);
        assert!(colors.palette_harmonious);
    }
}

#[test]
fn appearance_cli_color_only_override_uses_the_legacy_dark_seed() {
    let source = r#"
            [appearance.light]
            theme = "Atom One Light"

            [appearance.dark]
            theme = "Dracula"

            [appearance.dark.colors]
            palette-generate = true
            palette-harmonious = true
        "#;
    let unchanged = load_with_overrides(source, &[]).expect("load without overrides");
    assert_ne!(unchanged.appearance.light, unchanged.appearance.dark);

    let config = load_with_overrides(
        source,
        &[
            "--background",
            "#101112",
            "--no-palette-generate",
            "--no-palette-harmonious",
        ],
    )
    .expect("apply color override");

    assert_eq!(config.appearance.light, config.appearance.dark);
    assert_eq!(config.appearance.dark.theme.as_deref(), Some("Dracula"));
    assert_eq!(
        config.appearance.dark.colors.background,
        Some(Color::from_hex("#101112").unwrap())
    );
    assert_eq!(
        config.appearance.dark.colors.foreground,
        Some(Color::from_hex("#f8f8f2").unwrap())
    );
    assert!(!config.appearance.dark.colors.palette_generate);
    assert!(!config.appearance.dark.colors.palette_harmonious);
}

#[test]
fn invalid_cli_theme_keeps_the_config_theme_error() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let config_path = directory.path().join("config.toml");
    std::fs::write(&config_path, "").expect("write config");
    let expected = format!(
        "theme \"No Such Theme\" not found in {} or built-in catalog",
        directory.path().join("themes").display()
    );
    let cli = Cli::try_parse_from([
        "bootty",
        "app",
        "--config",
        config_path.to_str().expect("UTF-8 config path"),
        "--theme",
        "No Such Theme",
    ])
    .expect("parse invalid theme override");

    let error = cli.load_config().expect_err("invalid theme must fail");

    assert_eq!(error.to_string(), expected);
}
