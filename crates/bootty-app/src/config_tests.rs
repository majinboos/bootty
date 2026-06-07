use super::*;
use crate::color::Color;
use indoc::indoc;
use proptest::prelude::*;
use rstest::rstest;
use std::{collections::BTreeMap, fs};

struct ConfigSandbox {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl ConfigSandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        Self { _dir: dir, path }
    }

    fn with_config(source: &str) -> Self {
        let sandbox = Self::new();
        sandbox.write("config.toml", source);
        sandbox
    }

    fn write(&self, relative_path: &str, source: &str) {
        let path = self._dir.path().join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, source).unwrap();
    }

    fn load(&self) -> Result<BoottyConfig, ConfigLoadError> {
        load_config_from_path(&self.path)
    }
}

fn load_config_source(source: &str) -> BoottyConfig {
    ConfigSandbox::with_config(source).load().unwrap()
}

fn integer_table(values: &BTreeMap<String, i64>) -> Table {
    let mut table = Table::new();
    for (key, value) in values {
        table.insert(key, toml_edit::value(*value));
    }
    table
}

proptest! {
    #[test]
    fn merge_toml_tables_preserves_base_keys_not_present_in_overlay(
        base in proptest::collection::btree_map("[a-z][a-z0-9]{0,5}", any::<i64>(), 0..12),
        overlay in proptest::collection::btree_map("[m-z][a-z0-9]{0,5}", any::<i64>(), 0..12),
    ) {
        let mut target = integer_table(&base);
        let overlay_table = integer_table(&overlay);

        merge_toml_tables(&mut target, overlay_table);

        for (key, value) in base {
            if !overlay.contains_key(&key) {
                prop_assert_eq!(target[&key].as_integer(), Some(value));
            }
        }
    }

    #[test]
    fn merge_toml_tables_uses_overlay_values_for_matching_scalar_keys(
        key in "[a-z][a-z0-9]{0,5}",
        base_value in any::<i64>(),
        overlay_value in any::<i64>(),
    ) {
        let mut target = integer_table(&BTreeMap::from([(key.clone(), base_value)]));
        let overlay = integer_table(&BTreeMap::from([(key.clone(), overlay_value)]));

        merge_toml_tables(&mut target, overlay);

        prop_assert_eq!(target[&key].as_integer(), Some(overlay_value));
    }

}

#[rstest]
#[case::self_cycle(&[("a.toml", "a.toml")], "a.toml")]
#[case::two_file_cycle(&[("a.toml", "b.toml"), ("b.toml", "a.toml")], "a.toml")]
#[case::cycle_after_acyclic_entry(
    &[("entry.toml", "a.toml"), ("a.toml", "b.toml"), ("b.toml", "a.toml")],
    "entry.toml"
)]
fn include_cycles_are_rejected(#[case] edges: &[(&str, &str)], #[case] entry: &str) {
    let dir = tempfile::tempdir().unwrap();
    for (source, target) in edges {
        fs::write(
            dir.path().join(source),
            format!(
                indoc! {r#"
                    include = ["{target}"]
                "#},
                target = target
            ),
        )
        .unwrap();
    }

    assert!(load_config_from_path(dir.path().join(entry)).is_err());
}

#[test]
fn merge_toml_tables_recurses_into_nested_tables() {
    let mut target = indoc! {r#"
        [window]
        title = "base"
        width = 1000
    "#}
    .parse::<DocumentMut>()
    .unwrap();
    let overlay = indoc! {r#"
        [window]
        title = "included"
    "#}
    .parse::<DocumentMut>()
    .unwrap();

    merge_toml_tables(target.as_table_mut(), overlay.into_table());

    assert_eq!(target["window"]["title"].as_str(), Some("included"));
    assert_eq!(target["window"]["width"].as_integer(), Some(1000));
}

#[rstest]
#[case(Some("/tmp/xdg"), Some("/tmp/home"), "/tmp/xdg/bootty/config.toml")]
#[case(None, Some("/tmp/home"), "/tmp/home/.config/bootty/config.toml")]
fn config_path_prefers_xdg_then_home(
    #[case] xdg: Option<&str>,
    #[case] home: Option<&str>,
    #[case] expected: &str,
) {
    assert_eq!(config_path_from_env(xdg, home), PathBuf::from(expected));
}

#[test]
fn missing_config_file_loads_current_defaults() {
    let sandbox = ConfigSandbox::new();

    let config = sandbox.load().unwrap();

    assert_eq!(config.window.title, "Bootty");
    assert_eq!(config.window.width, 1220.0);
    assert_eq!(config.window.height, 760.0);
    assert_eq!(config.config_path, sandbox.path);
}

#[test]
fn included_file_overrides_containing_file_without_dropping_base_keys() {
    let sandbox = ConfigSandbox::with_config(indoc! {r#"
        include = ["local.toml"]

        [window]
        title = "base"
        width = 1000
    "#});
    sandbox.write(
        "local.toml",
        indoc! {r#"
            [window]
            title = "local"
            height = 640
        "#},
    );

    let config = sandbox.load().unwrap();

    assert_eq!(config.window.title, "local");
    assert_eq!(config.window.width, 1000.0);
    assert_eq!(config.window.height, 640.0);
}

#[test]
fn config_file_snapshot_changes_when_included_file_changes() {
    let sandbox = ConfigSandbox::with_config(indoc! {r#"
        include = ["local.toml"]
    "#});
    sandbox.write(
        "local.toml",
        indoc! {r#"
            [window]
            title = "before"
        "#},
    );

    let before = config_file_snapshot(&sandbox.path).unwrap();
    sandbox.write(
        "local.toml",
        indoc! {r#"
            [window]
            title = "after"
            width = 900
        "#},
    );
    let after = before.refresh_known_paths();

    assert_ne!(before, after);
}

#[rstest]
#[case::builtin_theme(
    indoc! {r#"
        theme = "Catppuccin Mocha"
    "#},
    Some("Catppuccin Mocha"),
    Some(Color::from_hex("#1e1e2e").unwrap()),
    Some(Color::from_hex("#cdd6f4").unwrap()),
    16
)]
#[case::explicit_color_override(
    indoc! {r##"
        theme = "Catppuccin Mocha"

        [colors]
        background = "#101112"
        palette = ["#000000", "#111111"]
    "##},
    Some("Catppuccin Mocha"),
    Some(Color::from_hex("#101112").unwrap()),
    Some(Color::from_hex("#cdd6f4").unwrap()),
    2
)]
fn config_resolves_theme_and_color_overrides(
    #[case] source: &str,
    #[case] theme: Option<&str>,
    #[case] background: Option<Color>,
    #[case] foreground: Option<Color>,
    #[case] palette_len: usize,
) {
    let config = load_config_source(source);

    assert_eq!(config.theme.as_deref(), theme);
    assert_eq!(config.colors.background, background);
    assert_eq!(config.colors.foreground, foreground);
    assert_eq!(config.colors.palette.len(), palette_len);
}

#[test]
fn user_theme_shadows_builtin_theme_name() {
    let sandbox = ConfigSandbox::with_config(indoc! {r#"
        theme = "Catppuccin Mocha"
    "#});
    sandbox.write(
        "themes/Catppuccin Mocha.toml",
        indoc! {r##"
            [metadata]
            name = "Catppuccin Mocha"
            source = "test sandbox"
            license = "test"

            [colors]
            background = "#000102"
            foreground = "#030405"
        "##},
    );

    let config = sandbox.load().unwrap();

    assert_eq!(
        config.colors.background,
        Some(Color::from_hex("#000102").unwrap())
    );
    assert_eq!(
        config.colors.foreground,
        Some(Color::from_hex("#030405").unwrap())
    );
    assert!(config.colors.palette.is_empty());
}

#[test]
fn missing_theme_reports_user_and_builtin_locations() {
    let error = ConfigSandbox::with_config(indoc! {r#"
        theme = "No Such Theme"
    "#})
    .load()
    .unwrap_err();

    assert!(error.to_string().contains("No Such Theme"));
    assert!(error.to_string().contains("themes"));
    assert!(error.to_string().contains("built-in catalog"));
}

#[rstest]
#[case("?missing.toml", true)]
#[case("missing.toml", false)]
fn missing_include_behavior_depends_on_optional_marker(
    #[case] include: &str,
    #[case] should_load: bool,
) {
    let sandbox = ConfigSandbox::with_config(&format!(
        indoc! {r#"
            include = ["{include}"]

            [window]
            title = "ok"
        "#},
        include = include
    ));

    match sandbox.load() {
        Ok(config) if should_load => assert_eq!(config.window.title, "ok"),
        Err(_) if !should_load => {}
        result => panic!("unexpected missing include result: {result:?}"),
    }
}

#[test]
fn reload_keeps_last_good_config_when_new_config_is_invalid() {
    let sandbox = ConfigSandbox::new();
    let good_path = sandbox._dir.path().join("good.toml");
    let bad_path = sandbox._dir.path().join("bad.toml");
    sandbox.write(
        "good.toml",
        indoc! {r#"
            [window]
            title = "good"
        "#},
    );
    fs::write(
        &bad_path,
        indoc! {r#"
            [window
            title = "bad"
        "#},
    )
    .unwrap();

    let mut state = ConfigState::new(load_config_from_path(&good_path).unwrap());

    let error = state.reload_from_path(&bad_path).unwrap_err();

    assert!(error.to_string().contains("bad.toml"));
    assert_eq!(state.current().window.title, "good");
    assert!(state.last_error().unwrap().contains("bad.toml"));
}

#[test]
fn config_document_preserves_comments_and_order_for_writeback() {
    let sandbox = ConfigSandbox::with_config(indoc! {r#"
        # user comment
        include = ["?local.toml"]

        [window]
        # title comment
        title = "Bootty"
        width = 1220
    "#});
    let source = fs::read_to_string(&sandbox.path).unwrap();

    let document = load_config_document(&sandbox.path).unwrap().unwrap();

    assert_eq!(document.path(), sandbox.path);
    assert_eq!(document.to_toml_string(), source);
}

#[test]
fn config_document_writeback_preserves_unrelated_comments_and_order() {
    let sandbox = ConfigSandbox::with_config(indoc! {r#"
        # user comment
        include = ["?local.toml"]

        [font]
        # size comment
        size = 13

        [chrome]
        sidebar = true
    "#});
    let mut document = load_config_document(&sandbox.path).unwrap().unwrap();

    document
        .set_item(&["font", "size"], toml_edit::value(15.0))
        .unwrap();
    document.write_to_disk().unwrap();

    let written = fs::read_to_string(&sandbox.path).unwrap();
    assert!(written.contains("# user comment"));
    assert!(written.contains("# size comment"));
    assert!(written.contains("[chrome]\nsidebar = true"));
    assert!(written.find("include").unwrap() < written.find("[font]").unwrap());
    assert!(written.contains("size = 15.0"));
}

#[test]
fn font_size_preference_writeback_round_trips_through_config_loader() {
    let sandbox = ConfigSandbox::with_config(indoc! {r#"
        # top comment
        [window]
        title = "Keep Me"
    "#});

    write_font_size_preference(&sandbox.path, 16.0).unwrap();

    let written = fs::read_to_string(&sandbox.path).unwrap();
    assert!(written.contains("# top comment"));
    assert!(written.find("[window]").unwrap() < written.find("[font]").unwrap());
    assert_eq!(
        load_config_from_path(&sandbox.path).unwrap().font.size,
        16.0
    );
}

#[test]
fn documented_sample_config_loads() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/sample-config.toml");

    let config = load_config_from_path(&path).unwrap();

    assert_eq!(config.theme.as_deref(), Some("Catppuccin Mocha"));
    assert!(
        config
            .input
            .keybind
            .contains(&"cmd+shift+r=reload_config".to_owned())
    );
    assert!(
        config
            .input
            .backend_keybinds
            .native
            .contains(&"cmd+n=new_mux_session".to_owned())
    );
    assert!(
        config
            .input
            .backend_keybinds
            .tmux
            .contains(&"cmd+n=csi:62~".to_owned())
    );
    assert_eq!(config.colors.palette.len(), 16);
}

#[test]
fn keybind_clear_directive_replaces_existing_bindings() {
    let config = load_config_source(indoc! {r#"
        version = 1

        [input]
        keybind = ["clear", "cmd+b=esc:090;8~"]
    "#});

    assert_eq!(config.input.keybind, vec!["cmd+b=esc:090;8~"]);
}

#[test]
fn config_accepts_native_multiplexer_backend() {
    let config = load_config_source(indoc! {r#"
        [multiplexer]
        backend = "native"
    "#});

    assert_eq!(config.multiplexer.backend, MultiplexerBackendConfig::Native);
}
