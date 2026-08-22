use std::sync::Arc;

use bootty_app::{
    command_extensions::{
        ExtensionHost, ModuleIdentity, editable_module_source, import_legacy_extension_module,
        legacy_extension_modules, module_identities, preview_module_surfaces, reset_module_source,
        save_module_source,
    },
    commands::{Caller, CommandCatalog, app_command_channel_with_repaint},
    config::{AppearanceVariant, BoottyConfig},
    control::ControlPlane,
    theme::theme_tokens,
};

fn app_command_channel(
    capacity: usize,
) -> (
    bootty_app::commands::AppCommandSender,
    bootty_app::commands::AppCommandReceiver,
) {
    app_command_channel_with_repaint(capacity, Arc::new(|| {}))
}

fn theme() -> Vec<(String, String)> {
    theme_tokens(&BoottyConfig::default(), AppearanceVariant::Dark)
}

#[test]
fn legacy_module_stays_in_place_until_explicit_validated_import() {
    let config = tempfile::tempdir().expect("config root");
    let legacy_root = config.path().join("status");
    std::fs::create_dir(&legacy_root).expect("legacy status root");
    let legacy_path = legacy_root.join("windows.luau");
    let legacy_source = "return function() return { text = 'legacy windows' } end\n";
    std::fs::write(&legacy_path, legacy_source).expect("legacy source");

    let legacy = legacy_extension_modules(config.path()).expect("legacy catalog");
    assert_eq!(legacy.len(), 1);
    assert_eq!(legacy[0].target_identity.as_str(), "windows.luau");
    let identity = import_legacy_extension_module(config.path(), &legacy[0], theme())
        .expect("validated import");
    assert_eq!(identity.as_str(), "windows.luau");
    assert_eq!(
        std::fs::read_to_string(&legacy_path).expect("legacy source remains"),
        legacy_source
    );

    let catalog = std::sync::Arc::new(CommandCatalog::default());
    let (sender, _receiver) = app_command_channel(4);
    let host = ExtensionHost::load(
        &config.path().join("extensions"),
        std::sync::Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        ControlPlane::default(),
    );
    assert_eq!(
        host.surface_items(
            bootty_app::command_extensions::SurfacePlacement::Status,
            "windows"
        )[0]
        .text,
        "legacy windows"
    );
}

#[test]
fn invalid_legacy_module_does_not_publish_an_import() {
    let config = tempfile::tempdir().expect("config root");
    let legacy_root = config.path().join("sidebar");
    std::fs::create_dir(&legacy_root).expect("legacy sidebar root");
    let legacy_path = legacy_root.join("broken.luau");
    std::fs::write(&legacy_path, "this is not valid luau").expect("legacy source");
    let legacy = legacy_extension_modules(config.path()).expect("legacy catalog");

    assert!(import_legacy_extension_module(config.path(), &legacy[0], theme()).is_err());
    assert!(!config.path().join("extensions").exists());
    assert_eq!(
        std::fs::read_to_string(legacy_path).expect("legacy source remains"),
        "this is not valid luau"
    );
}

#[test]
fn canonical_module_identity_supports_nested_paths_and_rejects_escape() {
    assert_eq!(
        ModuleIdentity::parse("nested/status.luau")
            .expect("nested identity")
            .as_str(),
        "nested/status.luau"
    );
    assert!(ModuleIdentity::parse("../status.luau").is_err());
    assert!(ModuleIdentity::parse("/tmp/status.luau").is_err());
    assert!(ModuleIdentity::parse("status.txt").is_err());
}

#[test]
fn explicit_surface_source_renders_with_deterministic_preview_facts() {
    let identity = ModuleIdentity::parse("preview.luau").expect("identity");
    let surfaces = preview_module_surfaces(
        &identity,
        r#"
            bootty.ui.register({ id = "preview", placement = "sidebar" }, function()
                local metrics = bootty.metrics()
                local sessions = bootty.sessions()
                return string.format("%.0f%% · %s · %d", metrics.cpu, sessions[1].name, sessions[1].ports[1])
            end)
        "#,
        theme(),
    )
    .expect("preview");

    assert_eq!(surfaces.len(), 1);
    assert_eq!(surfaces[0].items[0].text, "42% · work/api · 3000");
}

#[test]
fn builtin_and_local_modules_share_one_recursive_catalog() {
    let root = tempfile::tempdir().expect("extension root");
    let identity = ModuleIdentity::parse("nested/custom.luau").expect("identity");
    save_module_source(
        root.path(),
        &identity,
        "bootty.ui.register({ id = 'custom', placement = 'status' }, function() return 'ok' end)",
    )
    .expect("save nested module");

    let identities = module_identities(root.path()).expect("catalog");
    assert!(identities.contains(&identity));
    assert!(identities.contains(&ModuleIdentity::parse("windows.luau").expect("builtin")));
    let source = editable_module_source(root.path(), &identity).expect("local source");
    assert!(source.customized);
    assert!(!source.has_builtin);
}

#[test]
fn builtin_override_reset_restores_the_same_identity() {
    let root = tempfile::tempdir().expect("extension root");
    let identity = ModuleIdentity::parse("windows.luau").expect("identity");
    let builtin = editable_module_source(root.path(), &identity).expect("builtin source");
    assert!(!builtin.customized);
    assert!(builtin.has_builtin);

    save_module_source(root.path(), &identity, "-- customized").expect("save override");
    assert!(
        editable_module_source(root.path(), &identity)
            .expect("override")
            .customized
    );
    reset_module_source(root.path(), &identity).expect("reset override");
    assert!(
        !editable_module_source(root.path(), &identity)
            .expect("restored builtin")
            .customized
    );
}

#[cfg(unix)]
#[test]
fn atomic_source_replacement_retains_existing_unix_mode() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("extension root");
    let identity = ModuleIdentity::parse("read-only.luau").expect("identity");
    let path = root.path().join(identity.as_ref());
    std::fs::write(&path, b"old").expect("write original");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444))
        .expect("make read only");

    save_module_source(root.path(), &identity, "new").expect("replace module");

    assert_eq!(std::fs::read(&path).expect("read replacement"), b"new");
    assert_eq!(
        std::fs::metadata(&path)
            .expect("stat replacement")
            .permissions()
            .mode()
            & 0o7777,
        0o444
    );
}

#[cfg(unix)]
#[test]
fn canonical_source_write_rejects_a_symlink_escape() {
    use std::os::unix::fs::symlink;

    let parent = tempfile::tempdir().expect("parent");
    let root = parent.path().join("extensions");
    std::fs::create_dir(&root).expect("extension root");
    let outside = parent.path().join("outside.luau");
    std::fs::write(&outside, "outside").expect("outside source");
    symlink(&outside, root.join("escape.luau")).expect("source symlink");
    let identity = ModuleIdentity::parse("escape.luau").expect("identity");

    let error = save_module_source(&root, &identity, "replacement").expect_err("escape rejected");
    assert_eq!(
        error.to_string(),
        "extension module path escapes extension root"
    );
    assert_eq!(
        std::fs::read_to_string(outside).expect("outside source"),
        "outside"
    );
}

#[cfg(unix)]
#[test]
fn an_in_root_source_alias_keeps_one_canonical_module_identity() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("extension root");
    save_module_source(
        root.path(),
        &ModuleIdentity::parse("real.luau").expect("real identity"),
        "bootty.ui.register({ id = 'real', placement = 'status' }, function() return 'real' end)",
    )
    .expect("real source");
    symlink("real.luau", root.path().join("alias.luau")).expect("source alias");

    let identities = module_identities(root.path()).expect("module identities");
    assert!(identities.contains(&ModuleIdentity::parse("real.luau").expect("real identity")));
    assert!(!identities.contains(&ModuleIdentity::parse("alias.luau").expect("alias identity")));
    let catalog = std::sync::Arc::new(CommandCatalog::default());
    let (sender, _receiver) = app_command_channel(4);
    let host = ExtensionHost::load(
        root.path(),
        catalog,
        sender.for_caller(Caller::Luau),
        ControlPlane::default(),
    );
    let surfaces = host
        .surfaces(bootty_app::command_extensions::SurfacePlacement::Status)
        .into_iter()
        .filter(|surface| surface.snapshot.declaration.id == "real")
        .collect::<Vec<_>>();
    assert_eq!(surfaces.len(), 1);
    assert_eq!(surfaces[0].module, "real.luau");
}
