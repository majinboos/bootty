use std::time::{Duration, Instant};

use bootty_app::{
    config::{AppearanceVariant, BoottyConfig},
    extensions::{
        ExtensionRuntime, ModuleKind, available_module_names, module_names, module_source,
        preview_builtin_module, preview_module_source, reset_module, save_module,
        valid_module_name,
    },
    theme::theme_tokens,
};

fn theme() -> Vec<(String, String)> {
    theme_tokens(&BoottyConfig::default(), AppearanceVariant::Dark)
}

#[test]
fn module_results_map_to_the_public_ui_item_contract() {
    let items = preview_module_source(
        r##"return function()
            return {
                { text = "ready", fg = "#89b4fa", icon = "check", action = "reload_config" },
                { text = "plain" },
            }
        end"##,
        "contract",
        &theme(),
    );

    assert_eq!(items.len(), 2);
    assert_eq!(items[0].text, "ready");
    assert_eq!(items[0].icon.as_deref(), Some("check"));
    assert_eq!(items[0].action.as_deref(), Some("reload_config"));
    assert_eq!(items[1].text, "plain");
}

#[test]
fn preview_host_data_is_deterministic_and_never_needs_a_live_backend() {
    let items = preview_module_source(
        r#"return function()
            local metrics = bootty.metrics()
            local sessions = bootty.sessions()
            return string.format(
                "%.0f%% · %s · %d · %s",
                metrics.cpu,
                sessions[1].name,
                sessions[1].ports[1],
                tostring(bootty.awake())
            )
        end"#,
        "preview",
        &theme(),
    );

    assert_eq!(items[0].text, "42% · work/api · 3000 · true");
}

#[test]
fn every_builtin_module_produces_a_renderable_preview() {
    for (kind, names) in [
        (
            ModuleKind::Status,
            &["windows", "clock", "session", "sysinfo"][..],
        ),
        (ModuleKind::Sidebar, &["sessions", "codexbar"][..]),
        (
            ModuleKind::Session,
            &[
                "diffs",
                "process",
                "agent",
                "directory",
                "branch",
                "ports",
                "progress",
            ][..],
        ),
    ] {
        for name in names {
            let items = preview_builtin_module(kind, name, &theme());
            assert!(!items.is_empty(), "empty {kind:?} preview for {name}");
            assert!(
                items.iter().all(|item| !item.text.contains("error")),
                "failed {kind:?} preview for {name}: {items:?}"
            );
        }
    }
}

#[test]
fn preview_execution_stops_runaway_source_within_the_fast_test_budget() {
    let started = Instant::now();
    let items = preview_module_source("return function() while true do end end", "preview", &[]);

    assert!(started.elapsed() < Duration::from_millis(250));
    assert!(items[0].text.contains("preview exceeded 50 ms"));
}

#[test]
fn module_vms_cannot_mutate_or_leak_shared_theme_state() {
    let theme = vec![("text".to_owned(), "#cdd6f4".to_owned())];
    let mutation = preview_module_source(
        "bootty.theme.text = '#000000'; return function() return 'bad' end",
        "mutator",
        &theme,
    );
    let reader = preview_module_source(
        "return function() return bootty.theme.text end",
        "reader",
        &theme,
    );

    assert!(mutation[0].text.contains("readonly"));
    assert_eq!(reader[0].text, "#cdd6f4");
}

#[test]
fn module_files_use_safe_names_and_explicit_override_lifecycle() {
    let directory = tempfile::tempdir().expect("module directory");
    assert!(valid_module_name("my-module_2"));
    assert!(!valid_module_name("../module"));
    assert!(!valid_module_name("module.lua"));

    let builtin =
        module_source(directory.path(), ModuleKind::Sidebar, "sessions").expect("builtin source");
    assert!(!builtin.customized);
    assert!(builtin.has_builtin);

    let edited = format!("{}\n-- customized", builtin.source);
    save_module(directory.path(), ModuleKind::Sidebar, "sessions", &edited).expect("save override");
    let overridden = module_source(directory.path(), ModuleKind::Sidebar, "sessions")
        .expect("overridden source");
    assert!(overridden.customized);
    assert_eq!(overridden.source, edited);

    reset_module(directory.path(), "sessions").expect("reset override");
    assert!(
        !module_source(directory.path(), ModuleKind::Sidebar, "sessions")
            .expect("restored builtin")
            .customized
    );
}

#[cfg(unix)]
#[test]
fn a_read_only_existing_module_is_atomically_replaceable_while_mode_is_retained() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("module directory");
    let path = directory.path().join("read-only.luau");
    let original = b"return function() return 'old' end";
    std::fs::write(&path, original).expect("write original module");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444))
        .expect("make module read-only");

    let replacement = "return function() return 'new' end";
    let saved = save_module(
        directory.path(),
        ModuleKind::Status,
        "read-only",
        replacement,
    )
    .expect("replace read-only module");

    assert_eq!(saved, path);
    assert_eq!(
        std::fs::read(&path).expect("read replacement"),
        replacement.as_bytes()
    );
    assert_eq!(
        std::fs::metadata(&path)
            .expect("stat replacement")
            .permissions()
            .mode()
            & 0o7777,
        0o444
    );
}

#[test]
fn module_catalogs_merge_builtin_and_user_modules_by_kind() {
    let directory = tempfile::tempdir().expect("module directory");
    save_module(
        directory.path(),
        ModuleKind::Status,
        "custom-status",
        "return function() return 'ok' end",
    )
    .expect("save custom module");

    assert!(available_module_names(directory.path()).contains(&"custom-status".to_owned()));
    assert!(module_names(directory.path(), ModuleKind::Status).contains(&"clock".to_owned()));
    assert!(module_names(directory.path(), ModuleKind::Sidebar).contains(&"sessions".to_owned()));
    assert!(module_names(directory.path(), ModuleKind::Session).contains(&"ports".to_owned()));
}

#[test]
fn the_runtime_detects_user_overrides_without_test_only_state() {
    let directory = tempfile::tempdir().expect("module directory");
    let runtime = ExtensionRuntime::spawn_status(
        directory.path().to_path_buf(),
        egui::Context::default(),
        Vec::new(),
    );

    assert!(!runtime.has_user_module("windows"));
    save_module(
        directory.path(),
        ModuleKind::Status,
        "windows",
        "return function() return '' end",
    )
    .expect("save override");
    assert!(runtime.has_user_module("windows"));
}
