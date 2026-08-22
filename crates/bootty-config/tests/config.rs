#![allow(clippy::needless_raw_string_hashes)]
#![allow(clippy::float_cmp)]

use assert_fs::prelude::*;
use bootty_config::color::Color;
use bootty_config::config::*;
use bootty_font::FontFeature;
use indoc::indoc;
use pretty_assertions::{assert_eq, assert_ne};
use rstest::rstest;
use std::path::{Path, PathBuf};

struct ConfigSandbox {
    dir: assert_fs::TempDir,
    path: PathBuf,
}

impl ConfigSandbox {
    fn new() -> Self {
        let dir = assert_fs::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        Self { dir, path }
    }

    fn with_config(source: &str) -> Self {
        let sandbox = Self::new();
        sandbox.write("config.toml", source);
        sandbox
    }

    fn write(&self, relative_path: &str, source: &str) {
        let path = self.dir.child(relative_path);
        assert_fs::fixture::ChildPath::new(path.path().parent().expect("config fixture parent"))
            .create_dir_all()
            .expect("create config fixture parent");
        path.write_str(source).expect("write config fixture");
    }

    fn load(&self) -> Result<BoottyConfig, ConfigLoadError> {
        load_config_from_path(&self.path)
    }
}

fn load_config_source(source: &str) -> BoottyConfig {
    ConfigSandbox::with_config(source).load().unwrap()
}

#[rstest]
#[case::self_cycle(&[("a.toml", "a.toml")], "a.toml")]
#[case::two_file_cycle(&[("a.toml", "b.toml"), ("b.toml", "a.toml")], "a.toml")]
#[case::cycle_after_acyclic_entry(
    &[("entry.toml", "a.toml"), ("a.toml", "b.toml"), ("b.toml", "a.toml")],
    "entry.toml"
)]
fn include_cycles_are_rejected(#[case] edges: &[(&str, &str)], #[case] entry: &str) {
    let dir = assert_fs::TempDir::new().unwrap();
    for (source, target) in edges {
        dir.child(source)
            .write_str(&format!(
                indoc! {r#"
                    include = ["{target}"]
                "#},
                target = target
            ))
            .expect("write cyclic include");
    }

    assert!(load_config_from_path(dir.path().join(entry)).is_err());
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
    assert_eq!(config.chrome.bottom_segments, Vec::new());
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

    // An empty list keeps the defaults: a sidebar with no modules has no session list at all, so
    // an empty one only ever means the file was damaged.
    let emptied = load_config_source(indoc! {r#"
        [sidebar]
        modules = []
        session-modules = []
    "#});
    assert_eq!(emptied.sidebar.modules, defaults.sidebar.modules);
    assert_eq!(
        emptied.sidebar.session_modules,
        defaults.sidebar.session_modules
    );
    assert!(!emptied.sidebar.session_modules_configured);
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

    for variant in [AppearanceVariant::Light, AppearanceVariant::Dark] {
        let colors = config.colors_for_appearance(variant);
        assert_eq!(config.theme_for_appearance(variant), theme);
        assert_eq!(colors.background, background);
        assert_eq!(colors.foreground, foreground);
        assert_eq!(colors.palette.len(), palette_len);
    }
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

    let colors_only = load_config_source("[colors]\nbackground = \"#101112\"\n");
    assert_eq!(colors_only.appearance.light, colors_only.appearance.dark);
    assert_eq!(
        colors_only.appearance.dark.theme.as_deref(),
        Some("Catppuccin Mocha")
    );
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
fn config_resolves_font_features_in_product_order() {
    let config = load_config_source(indoc! {r#"
        font-feature = ["cv01", "ss05"]

        [font]
        features = ["cv33", "-calt"]
    "#});

    let features = config.font.features;

    assert_eq!(
        features,
        vec![
            FontFeature::new(*b"liga", 1),
            FontFeature::new(*b"cv33", 1),
            FontFeature::new(*b"calt", 0),
            FontFeature::new(*b"cv01", 1),
            FontFeature::new(*b"ss05", 1),
        ]
    );
}

#[test]
fn config_rejects_invalid_font_features() {
    let error = ConfigSandbox::with_config(indoc! {r#"
        [font]
        features = ["toolong"]
    "#})
    .load()
    .unwrap_err();

    assert_eq!(error.to_string(), "invalid font feature: toolong");
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

    for variant in [AppearanceVariant::Light, AppearanceVariant::Dark] {
        let colors = config.colors_for_appearance(variant);
        assert_eq!(
            colors.pointer_foreground,
            Some(Color::from_hex("#010203").unwrap())
        );
        assert_eq!(
            colors.highlight_foreground,
            Some(Color::from_hex("#131415").unwrap())
        );
        assert_eq!(
            colors.pointer_background,
            Some(Color::from_hex("#040506").unwrap())
        );
        assert_eq!(
            colors.tektronix_foreground,
            Some(Color::from_hex("#070809").unwrap())
        );
        assert_eq!(
            colors.tektronix_background,
            Some(Color::from_hex("#0a0b0c").unwrap())
        );
        assert_eq!(
            colors.highlight_background,
            Some(Color::from_hex("#0d0e0f").unwrap())
        );
        assert_eq!(
            colors.tektronix_cursor,
            Some(Color::from_hex("#101112").unwrap())
        );
    }
}

#[test]
fn named_ssh_profile_parses_structured_connection_fields() {
    let config = load_config_source(indoc! {r#"
        [ssh-profiles.local-mac]
        name = "Local Mac"
        host = "localhost"
        user = "luan"
        port = 2222
        host-key-policy = "accept-new"
        authentication = "key-file"
        identity-file = "/tmp/local-mac-key"
        proxy-jump = "gateway"
    "#});

    let profile = &config.ssh_profiles["local-mac"];
    assert_eq!(profile.name, "Local Mac");
    assert_eq!(profile.authentication, SshAuthenticationConfig::KeyFile);
    assert_eq!(
        profile.to_remote(),
        SshRemoteConfig {
            host: "localhost".to_owned(),
            user: Some("luan".to_owned()),
            port: Some(2222),
            program: "ssh".to_owned(),
            args: vec![
                "-J".to_owned(),
                "gateway".to_owned(),
                "-o".to_owned(),
                "StrictHostKeyChecking=accept-new".to_owned(),
                "-i".to_owned(),
                "/tmp/local-mac-key".to_owned(),
                "-o".to_owned(),
                "IdentitiesOnly=yes".to_owned(),
            ],
        }
    );
}

#[test]
fn ssh_agent_profile_selects_one_agent_identity_without_password_fallbacks() {
    let config = load_config_source(indoc! {r#"
        [ssh-profiles.agent]
        name = "Agent"
        host = "example.test"
        authentication = "agent"
        identity-file = "/tmp/agent-key.pub"
    "#});

    assert_eq!(
        config.ssh_profiles["agent"].to_remote().args,
        vec![
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "PreferredAuthentications=publickey",
            "-o",
            "PasswordAuthentication=no",
            "-o",
            "KbdInteractiveAuthentication=no",
            "-i",
            "/tmp/agent-key.pub",
            "-o",
            "IdentitiesOnly=yes",
        ]
    );
}

#[test]
fn ssh_agent_profile_requires_an_identity_reference() {
    let error = ConfigSandbox::with_config(indoc! {r#"
        [ssh-profiles.broken-agent]
        name = "Broken Agent"
        host = "example.test"
        authentication = "agent"
    "#})
    .load()
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("ssh-profiles.broken-agent.identity-file")
    );
}

#[test]
fn key_file_profile_requires_an_identity_file() {
    let error = ConfigSandbox::with_config(indoc! {r#"
        [ssh-profiles.broken]
        name = "Broken"
        host = "example.test"
        authentication = "key-file"
    "#})
    .load()
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("ssh-profiles.broken.identity-file")
    );
}

#[test]
fn extension_settings_load_into_their_own_module_table() {
    // Before this table existed, deny_unknown_fields made any extension key fail the whole config.
    let sandbox = ConfigSandbox::with_config(indoc! {r#"
        [extensions.greeter]
        greeting = "hello"
        loud = true
        repeats = 3
    "#});

    let config = sandbox
        .load()
        .expect("config with extension settings loads");

    let greeter = config.extensions.get("greeter").expect("module table");
    assert_eq!(
        greeter.get("greeting"),
        Some(&ExtensionSettingValue::Text("hello".to_owned()))
    );
    assert_eq!(
        greeter.get("loud"),
        Some(&ExtensionSettingValue::Bool(true))
    );
    assert_eq!(
        greeter.get("repeats"),
        Some(&ExtensionSettingValue::Number(3.0))
    );
    assert!(!config.extensions.contains_key("someone-else"));
}

#[test]
fn theme_catalog_combines_builtin_and_user_themes() {
    let sandbox = ConfigSandbox::new();
    sandbox.write("themes/My Theme.toml", "");
    sandbox.write("themes/ignored.txt", "");
    sandbox.write("themes/catppuccin mocha.toml", "");

    let names = available_theme_names(&sandbox.path);

    assert!(names.iter().any(|name| name == "My Theme"));
    assert!(!names.iter().any(|name| name == "ignored"));
    // A user copy of a built-in name replaces it instead of listing the theme twice.
    assert_eq!(
        names
            .iter()
            .filter(|name| name.eq_ignore_ascii_case("catppuccin mocha"))
            .count(),
        1
    );
    assert!(names.iter().any(|name| name == "Dracula"));
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

    for variant in [AppearanceVariant::Light, AppearanceVariant::Dark] {
        let colors = config.colors_for_appearance(variant);
        assert_eq!(colors.background, Some(Color::from_hex("#000102").unwrap()));
        assert_eq!(colors.foreground, Some(Color::from_hex("#030405").unwrap()));
        assert_eq!(colors.palette, Vec::new());
    }
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
fn documented_sample_config_loads() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/sample-config.toml");

    let config = load_config_from_path(&path).unwrap();

    for variant in [AppearanceVariant::Light, AppearanceVariant::Dark] {
        assert_eq!(
            config.theme_for_appearance(variant),
            Some("Catppuccin Mocha")
        );
    }
}

#[test]
fn obsolete_chrome_window_tabs_key_is_ignored() {
    let config = load_config_source(indoc! {r#"
        [chrome]
        window-tabs = true
    "#});
    assert_eq!(config.chrome, BoottyConfig::default().chrome);
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
fn split_keybind_entry_preserves_equals_key_semantics() {
    assert_eq!(
        split_keybind_entry("cmd+b=new_tab"),
        Some(("cmd+b", "new_tab"))
    );
    assert_eq!(
        split_keybind_entry("cmd+=increase_font_size:1"),
        Some(("cmd+", "increase_font_size:1"))
    );
    assert_eq!(
        split_keybind_entry("cmd+==increase_font_size:1"),
        Some(("cmd+=", "increase_font_size:1"))
    );
    assert_eq!(split_keybind_entry("cmd+v"), None);
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
fn ssh_remote_defaults_and_all_fields_are_preserved() {
    let defaults = load_config_source(indoc! {r#"
        [multiplexer]
        backend = "tmux"

        [multiplexer.remote]
        host = "devbox"
    "#});
    assert_eq!(
        defaults.multiplexer.remote,
        Some(SshRemoteConfig::for_host("devbox"))
    );

    let explicit = load_config_source(indoc! {r#"
        [multiplexer]
        backend = "tmux"

        [multiplexer.remote]
        host = "10.0.0.4"
        user = "dev"
        port = 2222
        program = "ssh-custom"
        args = ["-i", "key"]
    "#});
    assert_eq!(
        explicit.multiplexer.remote,
        Some(SshRemoteConfig {
            host: "10.0.0.4".to_owned(),
            user: Some("dev".to_owned()),
            port: Some(2222),
            program: "ssh-custom".to_owned(),
            args: vec!["-i".to_owned(), "key".to_owned()],
        })
    );
}

/// Only the backends bootty drives through a client can run on another host. The native backend
/// owns its terminals in this process, so accepting it would start local shells and present them as
/// the remote host's sessions.
#[test]
fn multiplexer_remote_is_refused_for_backends_with_no_remote_client() {
    for (backend, accepted) in [("tmux", true), ("rmux", true), ("native", false)] {
        let loaded = ConfigSandbox::with_config(&format!(
            "[multiplexer]\nbackend = \"{backend}\"\n\n[multiplexer.remote]\nhost = \"devbox\"\n"
        ))
        .load();

        assert_eq!(loaded.is_ok(), accepted, "backend {backend}");
    }
}

#[test]
fn multiplexer_remote_validation_errors_keep_their_exact_text() {
    let empty_host = ConfigSandbox::with_config(
        "[multiplexer]\nbackend = \"tmux\"\n\n[multiplexer.remote]\nhost = \"  \"\n",
    )
    .load()
    .unwrap_err();
    assert_eq!(
        empty_host.to_string(),
        "multiplexer.remote.host must name a host"
    );

    let unsupported = ConfigSandbox::with_config(
        "[multiplexer]\nbackend = \"native\"\n\n[multiplexer.remote]\nhost = \"devbox\"\n",
    )
    .load()
    .unwrap_err();
    assert_eq!(
        unsupported.to_string(),
        "multiplexer.remote needs a backend with a client to run there, got Native"
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
    assert_ne!(default_input.backend_keybinds.rmux, Vec::<String>::new());
    assert_eq!(
        default_input.backend_keybinds.rmux,
        default_input.backend_keybinds.native
    );
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

#[rstest]
#[case("bootty")]
#[case("tmux")]
fn prefixed_presets_use_alt_shift_for_direct_pane_bindings(#[case] preset: &str) {
    let config = load_config_source(&format!("[input]\npreset = \"{preset}\"\n"));

    for entry in [
        "alt+shift+h=select_pane:left",
        "alt+shift+j=select_pane:down",
        "alt+shift+k=select_pane:up",
        "alt+shift+l=select_pane:right",
        "alt+shift+o=next_pane",
        "alt+shift+x=kill_pane",
        "alt+shift+z=toggle_pane_zoom",
    ] {
        assert!(
            config.input.keybind.iter().any(|keybind| keybind == entry),
            "missing {preset} preset keybind {entry}"
        );
    }

    assert!(
        !config.input.keybind.iter().any(|entry| {
            [
                "alt+h=select_pane:left",
                "alt+j=select_pane:down",
                "alt+k=select_pane:up",
                "alt+l=select_pane:right",
                "alt+o=next_pane",
                "alt+x=kill_pane",
                "alt+z=toggle_pane_zoom",
            ]
            .contains(&entry.as_str())
        }),
        "{preset} preset still contains a bare-alt pane binding"
    );
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
    assert_eq!(config.input.backend_keybinds.tmux, Vec::<String>::new());
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
