use super::keybind_presets::{
    BOOTTY_PREFIX_KEYBINDS, TMUX_PREFIX_KEYBINDS, common_keybinds_macos, common_keybinds_other,
    common_keybinds_windows, ghostty_common_keybinds_macos, ghostty_common_keybinds_other,
    ghostty_layout_keybinds_macos, ghostty_layout_keybinds_other, native_scroll_keybinds_macos,
    native_scroll_keybinds_other, navigation_keybinds, prefix_passthrough_keybind,
    prefixed_keybinds, tmux_keybinds,
};
use super::*;
use crate::color::Color;
use bootty_winit::input_binding::{
    BindingAction, BindingElement, BindingTrigger, parse_binding_elements,
};
use bootty_winit::input_binding_set::BindingSet;
use indoc::indoc;
use proptest::prelude::*;
use rstest::rstest;
use std::str::FromStr;
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
fn empty_session_working_directory_resolves_to_home() {
    let expected_home = default_working_directory().expect("home directory should be discoverable");

    assert_eq!(
        SessionConfig::default().launch_config().working_directory,
        Some(expected_home)
    );
}

#[cfg(windows)]
#[test]
fn windows_default_working_directory_uses_userprofile_without_home() {
    let home = default_working_directory_from(|name| match name {
        "USERPROFILE" => Some("C:\\Users\\bootty".into()),
        _ => None,
    });

    assert_eq!(home, Some(PathBuf::from("C:\\Users\\bootty")));
}

#[cfg(windows)]
#[test]
fn windows_default_working_directory_uses_home_drive_and_path_without_userprofile() {
    let home = default_working_directory_from(|name| match name {
        "HOMEDRIVE" => Some("C:".into()),
        "HOMEPATH" => Some("\\Users\\bootty".into()),
        _ => None,
    });

    assert_eq!(home, Some(PathBuf::from("C:\\Users\\bootty")));
}

#[test]
fn configured_session_working_directory_overrides_home_default() {
    let config = SessionConfig {
        working_directory: Some(PathBuf::from("tmp/bootty-project")),
        ..SessionConfig::default()
    };

    assert_eq!(
        config.launch_config().working_directory,
        Some(PathBuf::from("tmp/bootty-project"))
    );
}

#[test]
fn missing_config_file_loads_with_selected_path() {
    let sandbox = ConfigSandbox::new();

    let config = sandbox.load().unwrap();

    assert_eq!(config.config_path, sandbox.path);
    assert_eq!(
        config.appearance.light.theme.as_deref(),
        Some("Catppuccin Latte")
    );
    assert_eq!(
        config.appearance.dark.theme.as_deref(),
        Some("Catppuccin Mocha")
    );
    assert_eq!(config.theme.as_deref(), Some("Catppuccin Mocha"));
}

#[test]
fn defaults_put_current_status_modules_in_visible_top_bar() {
    let config = load_config_source("");
    let modules = config
        .chrome
        .top_segments
        .iter()
        .map(|segment| segment.module.as_str())
        .collect::<Vec<_>>();
    let session = modules
        .iter()
        .position(|module| *module == "session")
        .expect("session status module is enabled by default");
    let windows = modules
        .iter()
        .position(|module| *module == "windows")
        .expect("windows status module is enabled by default");

    assert!(config.chrome.top_bar);
    assert!(!config.chrome.bottom_bar);
    assert!(config.chrome.bottom_segments.is_empty());
    assert!(session < windows, "session should appear before windows");
}

#[test]
fn sidebar_modules_default_and_override_in_order() {
    let defaults = load_config_source("");
    assert_eq!(defaults.sidebar.modules, ["sessions", "codexbar"]);
    assert_eq!(
        defaults.sidebar.session_modules,
        [
            "diffs",
            "process",
            "agent",
            "directory",
            "branch",
            "ports",
            "progress"
        ]
    );
    assert!(!defaults.sidebar.session_modules_configured);

    let configured = load_config_source(indoc! {r#"
        [sidebar]
        modules = ["custom", "sessions"]
        session-modules = ["directory", "progress"]
    "#});
    assert_eq!(configured.sidebar.modules, ["custom", "sessions"]);
    assert!(configured.sidebar.session_modules_configured);
    assert_eq!(
        configured.sidebar.session_modules,
        ["directory", "progress"]
    );
}

#[test]
fn chrome_bars_configure_visibility_and_modules_independently() {
    let config = load_config_source(indoc! {r#"
        [chrome]
        top-bar = false
        bottom-bar = true

        [[chrome.top-segment]]
        module = "clock"

        [[chrome.bottom-segment]]
        module = "sysinfo"
    "#});

    assert!(!config.chrome.top_bar);
    assert!(config.chrome.bottom_bar);
    assert_eq!(config.chrome.top_segments[0].module, "clock");
    assert_eq!(config.chrome.bottom_segments[0].module, "sysinfo");
}

#[test]
fn legacy_status_bar_config_maps_to_the_top_bar() {
    let config = load_config_source(indoc! {r#"
        [chrome]
        status-bar = false

        [[chrome.status-segment]]
        module = "clock"
    "#});

    assert!(!config.chrome.top_bar);
    assert_eq!(config.chrome.top_segments[0].module, "clock");
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
fn appearance_branches_resolve_separate_themes_and_overrides() {
    let config = load_config_source(indoc! {r##"
        [appearance]
        mode = "light"

        [appearance.light]
        theme = "Atom One Light"

        [appearance.light.colors]
        background = "#fefefe"

        [appearance.dark]
        theme = "Dracula"
    "##});

    assert_eq!(config.appearance.mode, AppearanceMode::Light);
    assert_eq!(
        config.appearance.light.theme.as_deref(),
        Some("Atom One Light")
    );
    assert_eq!(
        config.appearance.light.colors.background,
        Some(Color::from_hex("#fefefe").unwrap())
    );
    assert_eq!(config.appearance.dark.theme.as_deref(), Some("Dracula"));
    assert_eq!(
        config.appearance.dark.colors.background,
        Some(Color::from_hex("#282a36").unwrap())
    );
}

#[test]
fn legacy_theme_and_colors_seed_appearance_branches() {
    let config = load_config_source(indoc! {r##"
        theme = "Catppuccin Mocha"

        [colors]
        background = "#101112"
    "##});

    for branch in [&config.appearance.light, &config.appearance.dark] {
        assert_eq!(branch.theme.as_deref(), Some("Catppuccin Mocha"));
        assert_eq!(
            branch.colors.background,
            Some(Color::from_hex("#101112").unwrap())
        );
    }
    assert_eq!(config.theme.as_deref(), Some("Catppuccin Mocha"));
    assert_eq!(config.colors, config.appearance.dark.colors);
}

#[test]
fn config_resolves_sidebar_and_status_chrome_colors() {
    let config = load_config_source(indoc! {r##"
        [chrome]
        status-background = "#090909"
        notched-fullscreen-black-chrome = false
        pane-focus-border-color = "#9e75c780"

        [sidebar]
        position = "right"
        background = "#11131a"
        foreground = "#cdd6f4"
        selected = "#2a2f3d"
        hover = "#1e222c"
        border = "#313244"
    "##});

    assert_eq!(config.sidebar.position, SidebarPosition::Right);
    assert_eq!(
        config.chrome.status_background,
        Some(Color::from_hex("#090909").unwrap())
    );
    assert_eq!(
        config.chrome.pane_focus_border_color,
        Some(Color::from_hex("#9e75c780").unwrap())
    );
    assert!(!config.chrome.notched_fullscreen_black_chrome);
    assert_eq!(
        config.sidebar.background,
        Some(Color::from_hex("#11131a").unwrap())
    );
    assert_eq!(
        config.sidebar.foreground,
        Some(Color::from_hex("#cdd6f4").unwrap())
    );
    assert_eq!(
        config.sidebar.selected,
        Some(Color::from_hex("#2a2f3d").unwrap())
    );
    assert_eq!(
        config.sidebar.hover,
        Some(Color::from_hex("#1e222c").unwrap())
    );
    assert_eq!(
        config.sidebar.border,
        Some(Color::from_hex("#313244").unwrap())
    );
}

#[test]
fn config_defaults_sidebar_to_left_without_overrides() {
    let config = load_config_source("");

    assert_eq!(config.sidebar.position, SidebarPosition::Left);
    assert_eq!(config.sidebar.background, None);
    assert_eq!(config.chrome.status_background, None);
    assert!(config.chrome.notched_fullscreen_black_chrome);
}

#[test]
fn legacy_sidebar_fullscreen_colors_are_accepted_but_ignored() {
    let config = load_config_source(indoc! {r##"
        [sidebar]
        fullscreen-background = "#000000"
        fullscreen-hover = "#111111"
    "##});

    assert_eq!(config.sidebar.background, None);
    assert_eq!(config.sidebar.hover, None);
}

#[test]
fn config_overrides_fullscreen_top_offset() {
    let config = load_config_source(indoc! {r#"
        [window]
        fullscreen-top-offset = 40
    "#});

    assert_eq!(config.window.fullscreen_top_offset, Some(40.0));
    // Absent key keeps auto-detection (None).
    assert_eq!(load_config_source("").window.fullscreen_top_offset, None);
}

#[test]
fn config_toggles_fullscreen_tabs_in_notch() {
    let config = load_config_source(indoc! {r#"
        [window]
        fullscreen-tabs-in-notch = false
    "#});

    assert!(!config.window.fullscreen_tabs_in_notch);
    // Defaults to on so the notch band is used out of the box.
    assert!(load_config_source("").window.fullscreen_tabs_in_notch);
}

#[test]
fn config_accepts_ghostty_palette_generation_settings() {
    let config = load_config_source(indoc! {r##"
        [colors]
        background = "#ffffff"
        foreground = "#000000"
        palette = ["#000000", "#111111"]
        palette-generate = true
        palette-harmonious = true
    "##});

    assert!(config.colors.palette_generate);
    assert!(config.colors.palette_harmonious);

    let terminal_colors = config.colors.terminal_color_config();
    assert!(terminal_colors.palette_generate);
    assert!(terminal_colors.palette_harmonious);
}

#[test]
fn config_maps_font_features_to_terminal_text_config() {
    let config = load_config_source(indoc! {r#"
        font-feature = ["cv01", "ss05"]

        [font]
        features = ["cv33", "-calt"]
    "#});

    let features = config.font.terminal_text_config().font_features;

    assert!(features.contains(&FontFeature::new(*b"liga", 1)));
    assert!(features.contains(&FontFeature::new(*b"cv01", 1)));
    assert!(features.contains(&FontFeature::new(*b"ss05", 1)));
    assert!(features.contains(&FontFeature::new(*b"cv33", 1)));
    assert!(features.contains(&FontFeature::new(*b"calt", 0)));
}

#[test]
fn config_maps_font_fit_cell_height_to_terminal_text_config() {
    assert!(load_config_source("").font.fit_cell_height);

    let config = load_config_source(indoc! {r#"
        [font]
        fit-cell-height = false
    "#});

    assert!(!config.font.fit_cell_height);
    assert!(!config.font.terminal_text_config().fit_cell_height);
}

#[test]
fn config_maps_font_fit_cell_width_to_terminal_text_config() {
    assert!(!load_config_source("").font.fit_cell_width);

    let config = load_config_source(indoc! {r#"
        [font]
        fit-cell-width = true
    "#});

    assert!(config.font.fit_cell_width);
    assert!(config.font.terminal_text_config().fit_cell_width);
}

#[test]
fn config_uses_auto_font_cell_metrics_until_width_or_height_is_configured() {
    let default = load_config_source("");
    assert_eq!(default.font.cell_width, None);
    assert_eq!(default.font.cell_height, None);

    let config = load_config_source(indoc! {r#"
        [font]
        cell-width = 11
        cell-height = 24
    "#});

    assert_eq!(config.font.cell_width, Some(11.0));
    assert_eq!(config.font.cell_height, Some(24.0));
}

#[test]
fn config_rejects_invalid_font_features() {
    let error = ConfigSandbox::with_config(indoc! {r#"
        [font]
        features = ["toolong"]
    "#})
    .load()
    .unwrap_err();

    assert!(error.to_string().contains("invalid font feature"));
}

#[test]
fn config_accepts_xterm_dynamic_color_slots() {
    let config = load_config_source(indoc! {r##"
        [colors]
        pointer-foreground = "#010203"
        pointer-background = "#040506"
        tektronix-foreground = "#070809"
        tektronix-background = "#0a0b0c"
        highlight-background = "#0d0e0f"
        tektronix-cursor = "#101112"
        highlight-foreground = "#131415"
    "##});

    assert_eq!(
        config.colors.pointer_foreground,
        Some(Color::from_hex("#010203").unwrap())
    );
    assert_eq!(
        config.colors.highlight_foreground,
        Some(Color::from_hex("#131415").unwrap())
    );

    let terminal_colors = config.colors.terminal_color_config();
    assert_eq!(
        terminal_colors.pointer_background,
        Some(Color::from_hex("#040506").unwrap().into())
    );
    assert_eq!(
        terminal_colors.tektronix_cursor,
        Some(Color::from_hex("#101112").unwrap().into())
    );
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
}

#[test]
fn input_hide_mouse_pointer_while_typing_defaults_on_and_can_be_disabled() {
    assert!(
        BoottyConfig::default()
            .input
            .hide_mouse_pointer_while_typing
    );

    let config = load_config_source(indoc! {r#"
        [input]
        hide-mouse-pointer-while-typing = false
    "#});

    assert!(!config.input.hide_mouse_pointer_while_typing);
}

#[test]
fn input_copy_on_select_defaults_off_and_can_be_enabled() {
    assert!(!BoottyConfig::default().input.copy_on_select);

    let config = load_config_source(indoc! {r#"
        [input]
        copy-on-select = true
    "#});

    assert!(config.input.copy_on_select);
}

#[test]
fn config_maps_macos_option_as_alt_to_terminal_session_config() {
    let config = load_config_source(indoc! {r#"
        [input]
        macos-option-as-alt = "right"
    "#});

    assert_eq!(
        config.input.macos_option_as_alt,
        MacosOptionAsAltConfig::Right
    );
    assert_eq!(
        config.terminal_session_config().macos_option_as_alt,
        MacosOptionAsAlt::Right
    );
    assert_eq!(
        BoottyConfig::default()
            .terminal_session_config()
            .macos_option_as_alt,
        MacosOptionAsAlt::Both
    );
}

#[test]
fn config_maps_session_scrollback_to_terminal_session_config() {
    let config = load_config_source(indoc! {r#"
        [session]
        max-scrollback = 0
    "#});

    assert_eq!(config.session.max_scrollback, 0);
    assert_eq!(config.terminal_session_config().max_scrollback, 0);
    assert_eq!(
        BoottyConfig::default()
            .terminal_session_config()
            .max_scrollback,
        NATIVE_MAX_SCROLLBACK
    );
}

#[test]
fn config_maps_cursor_defaults_to_terminal_session_config() {
    let config = load_config_source(indoc! {r#"
        [cursor]
        style = "underline"
        blink = true
    "#});

    assert_eq!(config.cursor.style, Some(CursorStyleConfig::Underline));
    assert_eq!(config.cursor.blink, Some(true));
    assert_eq!(
        config.terminal_session_config().cursor,
        bootty_terminal::terminal_engine::TerminalCursorConfig {
            style: Some(bootty_terminal::terminal_engine::TerminalCursorStyle::Underline),
            blink: Some(true),
        }
    );
    assert_eq!(
        BoottyConfig::default().terminal_session_config().cursor,
        bootty_terminal::terminal_engine::TerminalCursorConfig::default()
    );
}

#[test]
fn config_maps_glyph_protocol_policy_to_terminal_features() {
    let config = load_config_source(indoc! {r#"
        [session]
        glyph-protocol = false
    "#});

    assert!(!config.session.glyph_protocol);
    assert!(!config.terminal_session_config().features.glyph_protocol);
    assert!(
        BoottyConfig::default()
            .terminal_session_config()
            .features
            .glyph_protocol
    );
}

#[test]
fn obsolete_chrome_window_tabs_key_is_ignored() {
    load_config_source(indoc! {r#"
        [chrome]
        window-tabs = true
    "#});
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
fn keybind_entries_without_clear_layer_on_defaults() {
    let config = load_config_source(indoc! {r#"
        version = 1

        [input]
        preset = "bootty"
        keybind = ["cmd+b=esc:090;8~"]
    "#});

    assert!(
        config.input.keybind.iter().any(|k| k == "cmd+b=esc:090;8~"),
        "user binding is kept"
    );
    assert!(
        config
            .input
            .keybind
            .iter()
            .any(|k| k == "shift+Enter=text:\\n"),
        "defaults the user did not list are retained"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_option_as_alt_expands_unsided_alt_keybinds_to_configured_sides() {
    let config = load_config_source(indoc! {r#"
        version = 1

        [input]
        macos-option-as-alt = "right"
        keybind = ["clear", "alt+n=next_tab", "left_alt+p=previous_tab"]
    "#});

    let keybinds = config
        .input
        .keybinds_for_backend(MultiplexerBackendConfig::Native);

    assert!(keybinds.iter().any(|entry| entry == "right_alt+n=next_tab"));
    assert!(!keybinds.iter().any(|entry| entry == "left_alt+n=next_tab"));
    assert!(
        keybinds
            .iter()
            .any(|entry| entry == "left_alt+p=previous_tab")
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_option_as_alt_preserves_command_alt_app_keybinds() {
    let config = load_config_source(indoc! {r#"
        version = 1

        [input]
        macos-option-as-alt = "none"
        keybind = ["clear", "cmd+alt+n=new_window", "cmd+alt+r=rename_session"]
    "#});

    let keybinds = config
        .input
        .keybinds_for_backend(MultiplexerBackendConfig::Native);

    assert!(keybinds.iter().any(|entry| entry == "cmd+alt+n=new_window"));
    assert!(
        keybinds
            .iter()
            .any(|entry| entry == "cmd+alt+r=rename_session")
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_option_as_alt_expands_non_command_steps_in_chains() {
    let config = load_config_source(indoc! {r#"
        version = 1

        [input]
        macos-option-as-alt = "right"
        keybind = ["clear", "cmd+k>alt+n=next_tab", "cmd+alt+p=previous_tab"]
    "#});

    let keybinds = config
        .input
        .keybinds_for_backend(MultiplexerBackendConfig::Native);

    assert!(
        keybinds
            .iter()
            .any(|entry| entry == "cmd+k>right_alt+n=next_tab")
    );
    assert!(
        keybinds
            .iter()
            .any(|entry| entry == "cmd+alt+p=previous_tab")
    );
}

#[test]
fn sidebar_keybind_clear_directive_replaces_existing_bindings() {
    let config = load_config_source(indoc! {r#"
        version = 1

        [input]
        sidebar-keybind = ["clear", "space=activate_session"]
    "#});

    assert_eq!(config.input.sidebar_keybind, vec!["space=activate_session"]);
}

#[test]
fn config_accepts_native_multiplexer_backend() {
    let config = load_config_source(indoc! {r#"
        [multiplexer]
        backend = "native"
    "#});

    assert_eq!(config.multiplexer.backend, MultiplexerBackendConfig::Native);
}

/// A host without a usable `~/.ssh/config` — the common case on Windows — has to be reachable from
/// the config file alone, so every connection detail the SSH client needs can be written here.
#[test]
fn multiplexer_remote_carries_the_connection_details_ssh_config_would_hold() {
    let config = load_config_source(indoc! {r#"
        [multiplexer]
        backend = "tmux"

        [multiplexer.remote]
        host = "10.0.0.4"
        user = "dev"
        port = 2222
        args = ["-i", "C:\\keys\\id_ed25519"]
    "#});

    let remote = config.multiplexer.remote.expect("remote");
    assert_eq!(remote.host, "10.0.0.4");
    assert_eq!(remote.user.as_deref(), Some("dev"));
    assert_eq!(remote.port, Some(2222));
    assert_eq!(remote.program, "ssh");
    assert_eq!(remote.args, vec!["-i", "C:\\keys\\id_ed25519"]);
}

/// Only the backends bootty drives through a client can run on another host. The native backend
/// owns its terminals in this process, so accepting it would start local shells and present them as
/// the remote host's sessions.
#[test]
fn multiplexer_remote_is_refused_for_backends_with_no_remote_client() {
    for (backend, accepted) in [
        ("tmux", true),
        ("zellij", true),
        ("rmux", true),
        ("native", false),
    ] {
        let loaded = ConfigSandbox::with_config(&format!(
            "[multiplexer]\nbackend = \"{backend}\"\n\n[multiplexer.remote]\nhost = \"devbox\"\n"
        ))
        .load();

        assert_eq!(loaded.is_ok(), accepted, "backend {backend}");
    }

    assert!(
        ConfigSandbox::with_config(
            "[multiplexer]\nbackend = \"tmux\"\n\n[multiplexer.remote]\nhost = \"  \"\n"
        )
        .load()
        .is_err()
    );
}

// Rmux is a native-layout backend: it must ship the same layout bindings as the native backend
// for every preset, or split/pane shortcuts silently vanish when switching backends.
#[test]
fn rmux_backend_defaults_mirror_native_layout_bindings() {
    let config = load_config_source(indoc! {r#"
        version = 1

        [input]
        preset = "bootty"
    "#});
    let keybinds = config
        .input
        .keybinds_for_backend(MultiplexerBackendConfig::Rmux);

    assert!(
        keybinds
            .iter()
            .any(|entry| entry == "ctrl+space>v=split_right")
    );
    assert!(
        keybinds
            .iter()
            .any(|entry| entry == "ctrl+space>-=split_down")
    );
    assert!(keybinds.iter().any(|entry| entry == "ctrl+space>c=new_tab"));

    let default_input = &BoottyConfig::default().input;
    assert!(!default_input.backend_keybinds.rmux.is_empty());
    assert_eq!(
        default_input.backend_keybinds.rmux,
        default_input.backend_keybinds.native
    );
}

#[test]
fn copy_mode_defaults_match_ghostty_bootty_and_tmux_entry_points() {
    assert!(ghostty_common_keybinds_macos().contains(&"cmd+y=copy_mode"));
    assert!(native_scroll_keybinds_macos().contains(&"cmd+y=copy_mode"));
    assert!(
        prefixed_keybinds("ctrl+space", BOOTTY_PREFIX_KEYBINDS)
            .iter()
            .any(|entry| entry == "ctrl+space>[=copy_mode")
    );
    assert!(
        prefixed_keybinds("ctrl+b", TMUX_PREFIX_KEYBINDS)
            .iter()
            .any(|entry| entry == "ctrl+b>[=copy_mode")
    );
}

// The platform default tables are cfg-selected, so these tests address both tables directly to
// validate the Linux/Windows table from any build host.
fn binding_triggers(entries: &[&str]) -> Vec<BindingTrigger> {
    entries
        .iter()
        .map(|entry| {
            parse_binding_elements(entry)
                .unwrap_or_else(|error| {
                    panic!("default keybind {entry:?} failed to parse: {error:?}")
                })
                .into_iter()
                .rev()
                .find_map(|element| match element {
                    BindingElement::Binding(binding) => Some(binding.trigger),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("default keybind {entry:?} has no binding trigger"))
        })
        .collect()
}

// Every shipped default keybind must parse; catches an invalid key token or action name (e.g. the
// tmux preset's shifted symbol keys or a future typo) before it reaches users.
#[test]
fn default_keybind_tables_parse() {
    let static_tables: &[&[&str]] = &[
        common_keybinds_macos(),
        common_keybinds_other(),
        common_keybinds_windows(),
        ghostty_common_keybinds_macos(),
        ghostty_common_keybinds_other(),
        ghostty_layout_keybinds_macos(),
        ghostty_layout_keybinds_other(),
        native_scroll_keybinds_macos(),
        native_scroll_keybinds_other(),
        tmux_keybinds(),
    ];
    let mut tables: Vec<Vec<String>> = static_tables
        .iter()
        .map(|table| owned_keybinds(table))
        .collect();
    tables.push(prefixed_keybinds("ctrl+space", BOOTTY_PREFIX_KEYBINDS));
    tables.push(prefixed_keybinds("ctrl+b", TMUX_PREFIX_KEYBINDS));
    tables.push(
        prefix_passthrough_keybind("ctrl+space")
            .into_iter()
            .collect(),
    );
    for table in tables {
        let mut set = BindingSet::default();
        for entry in &table {
            set.parse_and_put(entry).unwrap_or_else(|error| {
                panic!("default keybind {entry:?} failed to parse: {error:?}")
            });
        }
    }
}

// Root cause of the Linux/Windows breakage: a `cmd`/`super` binding maps to the Super/Windows key,
// which the desktop environment swallows. The non-macOS defaults must never require it.
#[test]
fn non_macos_default_keybinds_never_require_super() {
    let mut entries = Vec::new();
    entries.extend_from_slice(common_keybinds_other());
    entries.extend_from_slice(common_keybinds_windows());
    entries.extend_from_slice(native_scroll_keybinds_other());
    for trigger in binding_triggers(&entries) {
        assert!(
            !trigger.mods.command,
            "non-macOS default {trigger:?} requires the Super key"
        );
    }
}

// A repeated trigger silently shadows another action; the hand-authored tables must keep every
// trigger unique (the `cmd+w`/`cmd+shift+w` style collisions that motivated separate tables).
#[test]
fn common_keybind_triggers_are_unique_per_platform() {
    for table in [
        common_keybinds_macos(),
        common_keybinds_other(),
        common_keybinds_windows(),
    ] {
        let triggers = binding_triggers(table);
        for (index, trigger) in triggers.iter().enumerate() {
            assert!(
                !triggers[index + 1..].contains(trigger),
                "duplicate trigger {trigger:?} in default keybinds"
            );
        }
    }
}

// A repeated trigger across a preset's merged Global+layout defaults silently shadows an action;
// every preset/platform combination must keep the full chord chain unique.
#[test]
fn preset_merged_default_triggers_are_unique() {
    let mut cases: Vec<(String, Vec<String>)> = Vec::new();
    for (platform, global, scroll) in [
        (
            "macos",
            common_keybinds_macos(),
            native_scroll_keybinds_macos(),
        ),
        (
            "other",
            common_keybinds_other(),
            native_scroll_keybinds_other(),
        ),
        (
            "windows",
            common_keybinds_windows(),
            native_scroll_keybinds_other(),
        ),
    ] {
        for (preset, table, prefix) in [
            ("bootty", BOOTTY_PREFIX_KEYBINDS, "ctrl+space"),
            ("tmux", TMUX_PREFIX_KEYBINDS, "ctrl+b"),
        ] {
            let mut merged = owned_keybinds(global);
            merged.extend(owned_keybinds(navigation_keybinds()));
            merged.extend(prefixed_keybinds(prefix, table));
            merged.extend(owned_keybinds(scroll));
            cases.push((format!("{preset}-{platform}"), merged));
        }
    }
    for (platform, global, layout) in [
        (
            "macos",
            ghostty_common_keybinds_macos(),
            ghostty_layout_keybinds_macos(),
        ),
        (
            "other",
            ghostty_common_keybinds_other(),
            ghostty_layout_keybinds_other(),
        ),
    ] {
        let mut merged = owned_keybinds(global);
        merged.extend(owned_keybinds(layout));
        cases.push((format!("ghostty-{platform}"), merged));
    }
    for (name, merged) in cases {
        let chains: Vec<Vec<BindingTrigger>> = merged
            .iter()
            .map(|entry| {
                parse_binding_elements(entry)
                    .unwrap()
                    .into_iter()
                    .filter_map(|element| match element {
                        BindingElement::Leader(trigger) => Some(trigger),
                        BindingElement::Binding(binding) => Some(binding.trigger),
                        BindingElement::Chain(_) => None,
                    })
                    .collect()
            })
            .collect();
        for (index, chain) in chains.iter().enumerate() {
            assert!(
                !chains[index + 1..].contains(chain),
                "duplicate trigger for {:?} in {name} preset defaults",
                merged[index]
            );
        }
    }
}

// Switching preset must swap the built-in default tables while user override rows keep layering
// on top; a regression here either loses the user's rows or leaves the old preset's chords live.
#[test]
fn preset_selects_default_tables_and_keeps_user_overrides_layered() {
    let config = load_config_source(indoc! {r#"
        version = 1

        [input]
        preset = "tmux"
        keybind = ["cmd+g=new_tab"]
    "#});

    let keybinds = config
        .input
        .keybinds_for_backend(MultiplexerBackendConfig::Native);
    assert!(keybinds.iter().any(|entry| entry == "ctrl+b>c=new_tab"));
    assert!(!keybinds.iter().any(|entry| entry == "ctrl+space>c=new_tab"));
    assert!(keybinds.iter().any(|entry| entry == "cmd+g=new_tab"));
    // tmux's send-prefix: prefix twice delivers the raw prefix byte to the terminal.
    assert!(
        keybinds
            .iter()
            .any(|entry| entry == "ctrl+b>ctrl+b=text:\\x02")
    );
    assert!(
        keybinds
            .iter()
            .any(|entry| entry == "ctrl+b>:=command_palette")
    );
    assert!(
        config.input.backend_keybinds.tmux.is_empty(),
        "tmux preset leaves the prefix unbound on the tmux backend so real tmux receives it"
    );
}

#[test]
fn bootty_preset_tab_navigation_defaults_use_left_alt_shift() {
    let config = load_config_source(indoc! {r#"
        version = 1

        [input]
        preset = "bootty"
    "#});

    assert!(
        config
            .input
            .keybind
            .iter()
            .any(|entry| entry == "left_alt+shift+n=next_tab")
    );
    assert!(
        config
            .input
            .keybind
            .iter()
            .any(|entry| entry == "left_alt+shift+p=previous_tab")
    );
    assert!(
        !config
            .input
            .keybind
            .iter()
            .any(|entry| entry == "alt+n=next_tab")
    );
    assert!(
        !config
            .input
            .keybind
            .iter()
            .any(|entry| entry == "alt+p=previous_tab")
    );
    assert!(
        !config
            .input
            .keybind
            .iter()
            .any(|entry| entry == "alt+shift+n=next_tab")
    );
    assert!(
        !config
            .input
            .keybind
            .iter()
            .any(|entry| entry == "alt+shift+p=previous_tab")
    );
}

#[test]
fn bootty_preset_move_tab_defaults_bind_both_option_sides() {
    let config = load_config_source(indoc! {r#"
        version = 1

        [input]
        preset = "bootty"
    "#});

    for entry in [
        "left_alt+shift+,=move_tab:-1",
        "right_alt+shift+,=move_tab:-1",
        "left_alt+shift+.=move_tab:1",
        "right_alt+shift+.=move_tab:1",
    ] {
        assert!(
            config.input.keybind.iter().any(|keybind| keybind == entry),
            "missing raw keybind {entry}"
        );
    }
    assert!(
        !config
            .input
            .keybind
            .iter()
            .any(|entry| entry == "alt+shift+,=move_tab:-1")
    );

    let keybinds = config
        .input
        .keybinds_for_backend(MultiplexerBackendConfig::Native);
    for entry in [
        "left_alt+shift+,=move_tab:-1",
        "right_alt+shift+,=move_tab:-1",
    ] {
        assert!(
            keybinds.iter().any(|keybind| keybind == entry),
            "missing resolved keybind {entry}"
        );
    }
}

#[test]
fn ghostty_preset_does_not_ship_move_tab_defaults() {
    for table in [
        ghostty_layout_keybinds_macos(),
        ghostty_layout_keybinds_other(),
    ] {
        assert!(
            table.iter().all(|entry| !entry.contains("move_tab")),
            "Ghostty move-tab defaults should stay user-configured only"
        );
    }
}

// A remapped prefix must rebuild every prefixed chord and follow the external-tmux passthrough;
// a regression leaves the old leader baked into the defaults.
#[test]
fn prefix_override_rebuilds_prefixed_chords() {
    let config = load_config_source(indoc! {r#"
        version = 1

        [input]
        preset = "bootty"
        prefix = "ctrl+a"
    "#});

    let keybinds = config
        .input
        .keybinds_for_backend(MultiplexerBackendConfig::Native);
    assert!(keybinds.iter().any(|entry| entry == "ctrl+a>v=split_right"));
    assert!(
        !keybinds
            .iter()
            .any(|entry| entry.starts_with("ctrl+space>"))
    );

    let tmux_keybinds = config
        .input
        .keybinds_for_backend(MultiplexerBackendConfig::Tmux);
    assert!(
        tmux_keybinds
            .iter()
            .any(|entry| entry == "ctrl+a=text:\\x01")
    );
    assert!(
        !tmux_keybinds
            .iter()
            .any(|entry| entry == "ctrl+space=text:\\x00")
    );
}

// Ghostty has no prefix concept: a configured prefix must not leak chords into its tables.
#[test]
fn ghostty_preset_ignores_prefix_and_ships_direct_combos() {
    let config = load_config_source(indoc! {r#"
        version = 1

        [input]
        preset = "ghostty"
        prefix = "ctrl+a"
    "#});

    assert_eq!(config.input.effective_prefix(), None);
    let keybinds = config
        .input
        .keybinds_for_backend(MultiplexerBackendConfig::Native);
    assert!(keybinds.iter().all(|entry| !entry.contains('>')));
    assert!(config.input.backend_keybinds.tmux.is_empty());
}

// An empty prefix (recorder cleared) must fall back to the preset default instead of producing
// `>key=` chords with an empty leader.
#[test]
fn empty_prefix_falls_back_to_preset_default() {
    let config = load_config_source(indoc! {r#"
        version = 1

        [input]
        preset = "bootty"
        prefix = ""
    "#});

    assert_eq!(
        config.input.effective_prefix().as_deref(),
        Some("ctrl+space")
    );
}

// Known-answer control-byte encoding for the external-tmux prefix passthrough.
#[rstest]
#[case("ctrl+space", Some("ctrl+space=text:\\x00"))]
#[case("ctrl+a", Some("ctrl+a=text:\\x01"))]
#[case("ctrl+b", Some("ctrl+b=text:\\x02"))]
#[case("cmd+a", None)]
#[case("ctrl+shift+a", None)]
fn prefix_passthrough_encodes_control_bytes(#[case] prefix: &str, #[case] expected: Option<&str>) {
    assert_eq!(prefix_passthrough_keybind(prefix).as_deref(), expected);
}

// Known-answer: the Linux/Windows session shortcuts resolve to the WezTerm-style Ctrl+Shift combos.
#[test]
fn non_macos_session_shortcuts_use_ctrl_shift() {
    let mut set = BindingSet::default();
    for entry in common_keybinds_other() {
        set.parse_and_put(entry).unwrap();
    }
    assert_eq!(
        set.get_trigger(&BindingAction::NewMuxSession),
        Some(&BindingTrigger::from_str("ctrl+shift+n").unwrap())
    );
    assert_eq!(
        set.get_trigger(&BindingAction::SelectSession(1)),
        Some(&BindingTrigger::from_str("ctrl+shift+1").unwrap())
    );
    assert_eq!(
        set.get_trigger(&BindingAction::NextSession),
        Some(&BindingTrigger::from_str("ctrl+shift+]").unwrap())
    );
}

#[test]
fn windows_paste_defaults_include_standard_terminal_shortcuts() {
    let mut set = BindingSet::default();
    for entry in common_keybinds_windows() {
        set.parse_and_put(entry).unwrap();
    }

    assert_eq!(
        set.get(&BindingTrigger::from_str("ctrl+v").unwrap())
            .map(|binding| &binding.action),
        Some(&BindingAction::PasteFromClipboard)
    );
    assert_eq!(
        set.get(&BindingTrigger::from_str("ctrl+shift+v").unwrap())
            .map(|binding| &binding.action),
        Some(&BindingAction::PasteFromClipboard)
    );
    assert_eq!(
        set.get(&BindingTrigger::from_str("shift+Insert").unwrap())
            .map(|binding| &binding.action),
        Some(&BindingAction::PasteFromClipboard)
    );
}
#[test]
fn preset_only_config_keeps_default_appearance() {
    let missing = ConfigSandbox::new().load().unwrap();
    let preset_only = load_config_source("[input]\npreset = \"tmux\"\n");
    assert_eq!(
        missing.appearance.light.theme,
        preset_only.appearance.light.theme
    );
    assert_eq!(
        missing.appearance.dark.theme,
        preset_only.appearance.dark.theme
    );
    assert_eq!(
        missing.colors_for_appearance(AppearanceVariant::Light),
        preset_only.colors_for_appearance(AppearanceVariant::Light)
    );
}
