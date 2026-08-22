#![allow(clippy::needless_raw_string_hashes)]
#![allow(clippy::float_cmp)]

use bootty_config::color::Color;
use bootty_config::config::*;
use bootty_font::FontFeature;
use indoc::indoc;
use rstest::rstest;
use std::{
    fs,
    path::{Path, PathBuf},
};

struct ConfigSandbox {
    dir: tempfile::TempDir,
    path: PathBuf,
}

impl ConfigSandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        Self { dir, path }
    }

    fn with_config(source: &str) -> Self {
        let sandbox = Self::new();
        sandbox.write("config.toml", source);
        sandbox
    }

    fn write(&self, relative_path: &str, source: &str) {
        let path = self.dir.path().join(relative_path);
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
fn config_resolves_font_fit_cell_height() {
    assert!(load_config_source("").font.fit_cell_height);

    let config = load_config_source(indoc! {r#"
        [font]
        fit-cell-height = false
    "#});

    assert!(!config.font.fit_cell_height);
}

#[test]
fn config_resolves_font_fit_cell_width() {
    assert!(!load_config_source("").font.fit_cell_width);

    let config = load_config_source(indoc! {r#"
        [font]
        fit-cell-width = true
    "#});

    assert!(config.font.fit_cell_width);
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

    assert_eq!(
        config.colors.pointer_foreground,
        Some(Color::from_hex("#010203").unwrap())
    );
    assert_eq!(
        config.colors.highlight_foreground,
        Some(Color::from_hex("#131415").unwrap())
    );

    assert_eq!(
        config.colors.pointer_background,
        Some(Color::from_hex("#040506").unwrap())
    );
    assert_eq!(
        config.colors.tektronix_cursor,
        Some(Color::from_hex("#101112").unwrap())
    );
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
    assert_eq!(config.colors.palette, Vec::new());
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

    update_config_document(&sandbox.path, |document| {
        document.set_str(&["window", "title"], "Bootty")
    })
    .unwrap();

    assert_eq!(fs::read_to_string(&sandbox.path).unwrap(), source);
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
    update_config_document(&sandbox.path, |document| {
        document.set_f32(&["font", "size"], 15.0)
    })
    .unwrap();

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
fn config_parses_macos_option_as_alt_policy() {
    let config = load_config_source(indoc! {r#"
        [input]
        macos-option-as-alt = "right"
    "#});

    assert_eq!(
        config.input.macos_option_as_alt,
        MacosOptionAsAltConfig::Right
    );
}

#[test]
fn config_parses_session_scrollback_policy() {
    let config = load_config_source(indoc! {r#"
        [session]
        max-scrollback = 0
    "#});

    assert_eq!(config.session.max_scrollback, 0);
}

#[test]
fn config_parses_cursor_policy() {
    let config = load_config_source(indoc! {r#"
        [cursor]
        style = "underline"
        blink = true
    "#});

    assert_eq!(config.cursor.style, Some(CursorStyleConfig::Underline));
    assert_eq!(config.cursor.blink, Some(true));
}

#[test]
fn config_parses_glyph_protocol_policy() {
    let config = load_config_source(indoc! {r#"
        [session]
        glyph-protocol = false
    "#});

    assert!(!config.session.glyph_protocol);
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
fn config_accepts_native_multiplexer_backend() {
    let config = load_config_source(indoc! {r#"
        [multiplexer]
        backend = "native"
    "#});

    assert_eq!(config.multiplexer.backend, MultiplexerBackendConfig::Native);
}

#[test]
fn config_accepts_every_multiplexer_backend_token() {
    for (token, backend) in [
        ("native", MultiplexerBackendConfig::Native),
        ("rmux", MultiplexerBackendConfig::Rmux),
        ("tmux", MultiplexerBackendConfig::Tmux),
        ("zellij", MultiplexerBackendConfig::Zellij),
    ] {
        let config = load_config_source(&format!("[multiplexer]\nbackend = \"{token}\"\n"));
        assert_eq!(config.multiplexer.backend, backend, "backend token {token}");
    }
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
