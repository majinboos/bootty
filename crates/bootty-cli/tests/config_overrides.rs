use std::ffi::OsString;

use assert_fs::{TempDir, fixture::PathChild, prelude::FileWriteStr};
use bootty_cli::Cli;
use bootty_config::{
    color::Color,
    config::{
        AppearanceMode, AppearanceVariant, BoottyConfig, MultiplexerBackendConfig, SshRemoteConfig,
    },
};
use bootty_font::FontFeature;
use clap::Parser;
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::{fixture, rstest};

#[fixture]
fn config_dir() -> TempDir {
    TempDir::new().expect("temporary config directory")
}

fn load_with_overrides(
    directory: &TempDir,
    source: &str,
    overrides: &[String],
) -> anyhow::Result<BoottyConfig> {
    let config_path = directory.child("config.toml");
    config_path.write_str(source).expect("write config");
    let mut arguments = vec![
        OsString::from("bootty"),
        OsString::from("app"),
        OsString::from("--config"),
        config_path.path().into(),
    ];
    arguments.extend(overrides.iter().map(OsString::from));
    Cli::try_parse_from(arguments)
        .expect("parse config overrides")
        .load_config()
}

fn arguments(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn opaque(r: u8, g: u8, b: u8) -> Color {
    Color {
        r,
        g,
        b,
        a: u8::MAX,
    }
}

#[rstest]
fn appearance_overrides_apply_to_both_variants(config_dir: TempDir) {
    let config = load_with_overrides(
        &config_dir,
        r#"
            [appearance]
            mode = "light"

            [appearance.light]
            theme = "Atom One Light"

            [appearance.dark]
            theme = "Dracula"
        "#,
        &arguments(&[
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
        ]),
    )
    .expect("apply appearance overrides");

    assert_eq!(config.appearance.mode, AppearanceMode::Light);
    assert_eq!(config.appearance.light, config.appearance.dark);
    let colors = config.colors_for_appearance(AppearanceVariant::Light);
    assert_eq!(
        config.theme_for_appearance(AppearanceVariant::Light),
        Some("Catppuccin Mocha")
    );
    assert_eq!(
        [
            colors.background,
            colors.foreground,
            colors.cursor,
            colors.cursor_text,
            colors.selection_background,
            colors.selection_foreground,
        ],
        [
            opaque(0x10, 0x11, 0x12),
            opaque(0x20, 0x21, 0x22),
            opaque(0x30, 0x31, 0x32),
            opaque(0x40, 0x41, 0x42),
            opaque(0x50, 0x51, 0x52),
            opaque(0x60, 0x61, 0x62),
        ]
        .map(Some)
    );
    assert_eq!(
        colors.palette,
        [opaque(0x01, 0x02, 0x03), opaque(0x04, 0x05, 0x06)]
    );
    assert!(colors.palette_generate && colors.palette_harmonious);
}

#[rstest]
fn color_only_override_uses_the_dark_variant_as_the_shared_seed(config_dir: TempDir) {
    let config = load_with_overrides(
        &config_dir,
        "[appearance.light]\ntheme='Atom One Light'\n[appearance.dark]\ntheme='Dracula'\n\
         [appearance.dark.colors]\npalette-generate=true\npalette-harmonious=true",
        &arguments(&[
            "--background",
            "#101112",
            "--no-palette-generate",
            "--no-palette-harmonious",
        ]),
    )
    .expect("apply color override");

    assert_eq!(config.appearance.light, config.appearance.dark);
    let dark = &config.appearance.dark;
    assert_eq!(
        (
            dark.theme.as_deref(),
            dark.colors.background,
            dark.colors.foreground,
            dark.colors.palette_generate,
            dark.colors.palette_harmonious,
        ),
        (
            Some("Dracula"),
            Some(opaque(0x10, 0x11, 0x12)),
            Some(opaque(0xf8, 0xf8, 0xf2)),
            false,
            false,
        )
    );
}

#[rstest]
fn invalid_theme_reports_the_searched_theme_directory(config_dir: TempDir) {
    let error = load_with_overrides(&config_dir, "", &arguments(&["--theme", "No Such Theme"]))
        .expect_err("invalid theme must fail");

    assert_eq!(
        error.to_string(),
        format!(
            "theme \"No Such Theme\" not found in {} or built-in catalog",
            config_dir.child("themes").path().display()
        )
    );
}

#[rstest]
fn invalid_font_feature_reports_the_rejected_value(config_dir: TempDir) {
    let error = load_with_overrides(&config_dir, "", &arguments(&["--font-feature", "toolong"]))
        .expect_err("invalid font feature must fail");

    assert_eq!(error.to_string(), "invalid font feature: toolong");
}

proptest! {
    /// Property: replacing an SSH host preserves every independently configured remote field.
    #[test]
    fn ssh_host_override_changes_only_the_host(host in "[a-z][a-z0-9-]{0,30}") {
        let directory = TempDir::new().expect("temporary config directory");
        let config = load_with_overrides(
            &directory,
            r#"
                [multiplexer]
                backend = "tmux"

                [multiplexer.remote]
                host = "old-host"
                user = "dev"
                port = 2222
                program = "ssh-wrapper"
                args = ["-i", "key"]
            "#,
            &["--ssh-remote".to_owned(), host.clone()],
        )
        .expect("apply SSH host override");

        assert_eq!(config.multiplexer.backend, MultiplexerBackendConfig::Tmux);
        assert_eq!(
            config.multiplexer.remote,
            Some(SshRemoteConfig {
                host,
                user: Some("dev".to_owned()),
                port: Some(2222),
                program: "ssh-wrapper".to_owned(),
                args: vec!["-i".to_owned(), "key".to_owned()],
            })
        );
    }
}

proptest! {
    /// Property: every RGB override reaches both appearance variants with opaque alpha unchanged.
    #[test]
    fn background_override_sets_the_same_exact_rgb_on_both_variants(
        red in any::<u8>(),
        green in any::<u8>(),
        blue in any::<u8>(),
    ) {
        let directory = TempDir::new().expect("temporary config directory");
        let argument = format!("#{red:02x}{green:02x}{blue:02x}");
        let config = load_with_overrides(
            &directory,
            "",
            &["--background".to_owned(), argument],
        )
        .expect("apply background override");
        let expected = Some(opaque(red, green, blue));

        assert_eq!(config.appearance.light.colors.background, expected);
        assert_eq!(config.appearance.dark.colors.background, expected);
    }
}

proptest! {
    /// Property: every valid four-byte feature and numeric value is appended without changing it.
    #[test]
    fn font_feature_override_appends_the_parsed_feature(
        tag in prop::array::uniform4(b'a'..=b'z'),
        value in 2_u32..=10_000,
    ) {
        let directory = TempDir::new().expect("temporary config directory");
        let tag_text = String::from_utf8(tag.to_vec()).expect("generated ASCII tag");
        let feature = format!("{tag_text}={value}");
        let config = load_with_overrides(
            &directory,
            "[font]\nfeatures = [\"-liga\"]\n",
            &["--font-feature".to_owned(), feature],
        )
        .expect("apply font feature override");

        let expected = vec![
            FontFeature::new(*b"liga", 1),
            FontFeature::new(*b"liga", 0),
            FontFeature::new(tag, value),
        ];
        assert_eq!(config.font.features, expected);
    }
}
