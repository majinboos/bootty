use std::{collections::BTreeMap, path::Path, sync::Arc};

use bootty_command::{Caller, app_command_channel};
use bootty_config::config::ExtensionSettingValue;
use bootty_extension::{
    ExtensionCatalog, ExtensionHost, ModuleIdentity, ModuleSourceOutcome, ModuleSourceRequest,
    SurfacePlacement, display_path, editable_module_source, event_queue,
    import_legacy_extension_module, legacy_extension_modules, module_identities, module_template,
    preview_module_surfaces, reset_module_source, save_module_source,
};

fn theme() -> Vec<(String, String)> {
    vec![
        ("accent".to_owned(), "#7aa2f7".to_owned()),
        ("text".to_owned(), "#c0caf5".to_owned()),
        ("subtext".to_owned(), "#a9b1d6".to_owned()),
    ]
}

#[test]
fn legacy_module_stays_in_place_until_explicit_validated_import() {
    let config = tempfile::tempdir().expect("config root");
    let legacy_root = config.path().join("status");
    std::fs::create_dir(&legacy_root).expect("legacy status root");
    let legacy_path = legacy_root.join("windows.luau");
    let legacy_source = "return function() return { text = 'legacy windows' } end\n";
    std::fs::write(&legacy_path, legacy_source).expect("legacy source");

    let catalog = Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4, Arc::new(|| {}));
    let mut host = ExtensionHost::load(
        &config.path().join("extensions"),
        catalog,
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );
    let legacy = host.module_sources().legacy.to_vec();
    assert_eq!(legacy.len(), 1);
    assert_eq!(legacy[0].target_identity.as_str(), "windows.luau");

    assert_eq!(
        host.apply_module_source_request(ModuleSourceRequest::ImportLegacy(legacy[0].clone())),
        ModuleSourceOutcome::Imported(Ok(ModuleIdentity::parse("windows.luau").expect("identity")))
    );
    assert_eq!(
        std::fs::read_to_string(&legacy_path).expect("legacy source remains"),
        legacy_source
    );
    let surface = host
        .surface(SurfacePlacement::Status, "windows")
        .expect("windows surface");
    assert_eq!(surface.snapshot.items[0].text, "legacy windows");
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
fn display_paths_contract_home_without_contracting_lookalikes() {
    let home = Path::new("/tmp/bootty-home");
    let child = home.join("src");
    let lookalike = std::path::PathBuf::from(format!("{}-backup", home.display()));

    assert_eq!(
        display_path(&child.to_string_lossy(), Some(home)),
        Path::new("~").join("src").display().to_string()
    );
    assert_eq!(
        display_path(&lookalike.to_string_lossy(), Some(home)),
        lookalike.display().to_string()
    );
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
    let catalog = Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4, Arc::new(|| {}));
    let host = ExtensionHost::load(
        root.path(),
        catalog,
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );
    let surfaces = host
        .surfaces(SurfacePlacement::Status)
        .into_iter()
        .filter(|surface| surface.snapshot.declaration.id == "real")
        .collect::<Vec<_>>();
    assert_eq!(surfaces.len(), 1);
    assert_eq!(surfaces[0].module, "real.luau");
}

#[test]
fn editor_requests_run_against_the_host_extension_root() {
    let config = tempfile::tempdir().expect("config root");
    let root = config.path().join("extensions");
    let broken = ModuleIdentity::parse("broken.luau").expect("identity");
    save_module_source(&root, &broken, "this is not luau").expect("write broken module");

    let catalog = Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4, Arc::new(|| {}));
    let mut host = ExtensionHost::load(
        &root,
        catalog,
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );
    // A module that fails to load stays listed, or it could never be edited back into shape.
    assert!(host.module_sources().identities.contains(&broken));

    let created = ModuleIdentity::parse("nested/extra.luau").expect("identity");
    assert_eq!(
        host.apply_module_source_request(ModuleSourceRequest::Create(
            "nested/extra.luau".to_owned()
        )),
        ModuleSourceOutcome::Created(Ok(created.clone()))
    );
    assert!(host.module_sources().identities.contains(&created));
    assert!(matches!(
        host.apply_module_source_request(ModuleSourceRequest::Create(
            "nested/extra.luau".to_owned()
        )),
        ModuleSourceOutcome::Created(Err(error)) if error.contains("already exists")
    ));

    let missing = ModuleIdentity::parse("missing.luau").expect("identity");
    let ModuleSourceOutcome::Loaded { source, exists } =
        host.apply_module_source_request(ModuleSourceRequest::Load(missing.clone()))
    else {
        panic!("load answers with a source");
    };
    assert!(!exists);
    assert_eq!(source.source, module_template(&missing));
    assert!(!root.join("missing.luau").exists());

    host.apply_module_source_request(ModuleSourceRequest::Save {
        identity: broken.clone(),
        source: "-- fixed".to_owned(),
    });
    let ModuleSourceOutcome::Loaded { source, exists } =
        host.apply_module_source_request(ModuleSourceRequest::Load(broken.clone()))
    else {
        panic!("load answers with a source");
    };
    assert!(exists);
    assert_eq!(source.source, "-- fixed");

    assert_eq!(
        host.apply_module_source_request(ModuleSourceRequest::Reset(broken.clone())),
        ModuleSourceOutcome::Reset(Ok(broken.clone()))
    );
    assert!(!host.module_sources().identities.contains(&broken));
}

#[test]
fn a_module_declares_and_reads_only_its_own_settings() {
    let root = tempfile::tempdir().expect("extension root");
    std::fs::write(
        root.path().join("themed.luau"),
        r#"
bootty.settings.register({ key = "greeting", label = "Greeting", default = "hi" })
bootty.settings.register({ key = "loud", default = false })
bootty.ui.register({ id = "themed", placement = "status" }, function()
    local other = bootty.settings.get("nothing-of-mine")
    return { { text = tostring(bootty.settings.get("greeting")) .. ":" .. tostring(other) } }
end)
"#,
    )
    .expect("write module");

    let catalog = Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4, Arc::new(|| {}));
    let host = ExtensionHost::load(
        root.path(),
        catalog,
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );

    let (declarations, revision) = host.setting_declarations();
    let declared: Vec<_> = declarations
        .iter()
        .filter(|declaration| declaration.module == "themed")
        .collect();
    assert_eq!(declared.len(), 2, "both settings are declared");
    // The module never names its namespace; the host stamps it from the module identity.
    assert_eq!(declared[0].key, "greeting");
    assert_eq!(declared[0].label, "Greeting");
    assert_eq!(
        declared[0].default,
        ExtensionSettingValue::Text("hi".to_owned())
    );
    assert_eq!(declared[1].default, ExtensionSettingValue::Bool(false));
    assert!(revision > 0, "declaring settings advances the revision");

    // A user value reaches the module, and another module's table stays invisible to it.
    let mut accepted = BTreeMap::new();
    accepted.insert(
        "themed".to_owned(),
        BTreeMap::from([(
            "greeting".to_owned(),
            ExtensionSettingValue::Text("hello".to_owned()),
        )]),
    );
    accepted.insert(
        "other".to_owned(),
        BTreeMap::from([(
            "nothing-of-mine".to_owned(),
            ExtensionSettingValue::Text("secret".to_owned()),
        )]),
    );
    host.update_settings(accepted);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut rendered = None;
    while std::time::Instant::now() < deadline {
        if let Some(surface) = host.surface(SurfacePlacement::Status, "themed")
            && let Some(item) = surface.snapshot.items.first()
            && item.text == "hello:nil"
        {
            rendered = Some(item.text.clone());
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        rendered.as_deref(),
        Some("hello:nil"),
        "the module reads its own accepted value and cannot see another module's"
    );
}
