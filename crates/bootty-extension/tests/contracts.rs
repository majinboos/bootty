use std::{fs, path::Path, process::Command, time::Duration};

use bootty_command::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, app_command_channel,
};
use bootty_extension::{
    ExtensionCatalog, ExtensionGenerationCandidate, ExtensionGenerationToken, ExtensionHost,
    ModuleColor, ModuleIdentity, ModuleItem, ModuleSourceOutcome, ModuleSourceRequest,
    SessionReorder, SurfaceDeclaration, SurfacePlacement, SurfaceSnapshot, event_queue,
    head_branch, module_identities, preview_builtin_surfaces,
};

fn git_ok(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(output.status.success());
}

fn git_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().expect("temporary repository root");
    let main = root.path().join("main");
    fs::create_dir(&main).expect("create main worktree");
    git_ok(&main, &["init", "-q", "-b", "main"]);
    git_ok(&main, &["config", "user.email", "test@bootty.dev"]);
    git_ok(&main, &["config", "user.name", "Bootty Test"]);
    fs::write(main.join("README"), "hello").expect("write initial file");
    git_ok(&main, &["add", "."]);
    git_ok(&main, &["commit", "-q", "-m", "init"]);
    (root, main)
}

fn surface(id: &str) -> SurfaceSnapshot {
    SurfaceSnapshot {
        declaration: SurfaceDeclaration {
            id: id.to_owned(),
            placement: SurfacePlacement::Status,
            order: 0,
            interval: Duration::from_secs(1),
            title: None,
            icon: None,
            hint: None,
        },
        items: vec![ModuleItem {
            text: "ok".to_owned(),
            fg: Some(ModuleColor::rgb(1, 2, 3)),
            ..ModuleItem::default()
        }],
    }
}

#[test]
fn identity_namespace_is_stable_and_host_neutral() {
    let identity = ModuleIdentity::parse("nested/probe.luau").expect("identity");
    assert_eq!(identity.as_str(), "nested/probe.luau");
    assert_eq!(identity.namespace(), "nested.probe");
    assert_eq!(ModuleColor::rgb(1, 2, 3).a, 255);
}

#[test]
fn replacement_retires_old_generation_and_rejects_stale_surface_publish() {
    let catalog = ExtensionCatalog::default();
    let identity = ModuleIdentity::parse("probe.luau").expect("identity");
    let first = ExtensionGenerationToken::new();
    catalog
        .publish_generation(ExtensionGenerationCandidate {
            identity: identity.clone(),
            generation: 1,
            token: first.clone(),
            commands: Vec::new(),
            topics: vec!["probe.events".to_owned()],
            surfaces: vec![surface("status")],
        })
        .expect("publish first generation");
    let second = ExtensionGenerationToken::new();
    catalog
        .publish_generation(ExtensionGenerationCandidate {
            identity: identity.clone(),
            generation: 2,
            token: second.clone(),
            commands: Vec::new(),
            topics: vec!["probe.events".to_owned()],
            surfaces: vec![surface("status")],
        })
        .expect("publish replacement generation");
    assert!(!first.is_active());
    assert!(second.is_active());
    assert!(
        catalog
            .publish_surfaces("probe.luau", 1, vec![surface("status")])
            .is_err()
    );
    assert_eq!(catalog.surfaces()[0].generation, 2);
}

#[test]
fn inactive_generation_publish_leaves_catalog_unchanged() {
    let catalog = ExtensionCatalog::default();
    let identity = ModuleIdentity::parse("probe.luau").expect("identity");
    catalog
        .publish_generation(ExtensionGenerationCandidate {
            identity: identity.clone(),
            generation: 1,
            token: ExtensionGenerationToken::new(),
            commands: Vec::new(),
            topics: vec!["probe.events".to_owned()],
            surfaces: vec![surface("status")],
        })
        .expect("publish first generation");
    let inactive = ExtensionGenerationToken::new();
    inactive.retire();

    assert_eq!(
        catalog.publish_generation(ExtensionGenerationCandidate {
            identity,
            generation: 2,
            token: inactive,
            commands: Vec::new(),
            topics: vec!["probe.other".to_owned()],
            surfaces: vec![surface("other")],
        }),
        Err("extension generation is not active".to_owned())
    );
    assert!(catalog.topics().contains("probe.events"));
    assert!(!catalog.topics().contains("probe.other"));
    assert_eq!(catalog.surfaces()[0].generation, 1);
}

#[test]
fn retired_generation_reorder_is_dropped_but_current_reorder_applies() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    let module = directory.path().join("reorder.luau");
    fs::write(
        &module,
        r#"
bootty.commands.register({ id = "reorder.old", title = "Old" }, function()
    bootty.reorder_session("old", nil)
end)
"#,
    )
    .expect("write old generation");
    let catalog = std::sync::Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4, std::sync::Arc::new(|| {}));
    let mut host = ExtensionHost::load(
        directory.path(),
        std::sync::Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );
    let (_, old_sender) = catalog.command("reorder.old").expect("old command");
    assert!(matches!(
        old_sender
            .invoke(
                CommandInvocation::new("reorder.old", Vec::new(), Caller::Socket),
                std::time::Instant::now() + Duration::from_secs(1),
                CommandCancellation::new(),
            )
            .recv_timeout(Duration::from_secs(1))
            .expect("old outcome"),
        CommandOutcome::Success { .. }
    ));

    fs::write(
        &module,
        r#"
bootty.commands.register({ id = "reorder.current", title = "Current" }, function()
    bootty.reorder_session("current", nil)
end)
"#,
    )
    .expect("write current generation");
    host.refresh(std::time::Instant::now() + Duration::from_secs(1));
    let reorders = host.take_session_reorders();
    assert_eq!(reorders, Vec::<SessionReorder>::new());

    let (_, current_sender) = catalog.command("reorder.current").expect("current command");
    assert!(matches!(
        current_sender
            .invoke(
                CommandInvocation::new("reorder.current", Vec::new(), Caller::Socket),
                std::time::Instant::now() + Duration::from_secs(1),
                CommandCancellation::new(),
            )
            .recv_timeout(Duration::from_secs(1))
            .expect("current outcome"),
        CommandOutcome::Success { .. }
    ));
    assert_eq!(
        host.take_session_reorders(),
        [SessionReorder {
            source: "current".to_owned(),
            before: None,
        }]
    );
}

#[test]
fn generation_declarations_require_nonempty_local_namespace_segments() {
    let catalog = ExtensionCatalog::default();
    let identity = ModuleIdentity::parse("probe.luau").expect("identity");

    catalog
        .publish_generation(ExtensionGenerationCandidate {
            identity: identity.clone(),
            generation: 1,
            token: ExtensionGenerationToken::new(),
            commands: Vec::new(),
            topics: vec!["probe.events".to_owned()],
            surfaces: Vec::new(),
        })
        .expect("publish valid namespaced declarations");
    assert!(catalog.topics().contains("probe.events"));

    for id in ["probe.", "probe..topic"] {
        assert!(
            catalog
                .publish_generation(ExtensionGenerationCandidate {
                    identity: identity.clone(),
                    generation: 2,
                    token: ExtensionGenerationToken::new(),
                    commands: Vec::new(),
                    topics: vec![id.to_owned()],
                    surfaces: Vec::new(),
                })
                .is_err()
        );
    }
    assert!(catalog.topics().contains("probe.events"));
}

#[test]
fn branch_labels_preserve_slashed_names_and_report_detached_heads() {
    let (_root, main) = git_repo();
    git_ok(&main, &["checkout", "-q", "-b", "one/two/three"]);
    let nested = main.join("nested");
    fs::create_dir(&nested).expect("create nested directory");
    assert_eq!(
        head_branch(main.to_str().unwrap()).as_deref(),
        Some("one/two/three")
    );
    assert_eq!(
        head_branch(nested.to_str().unwrap()).as_deref(),
        Some("one/two/three")
    );

    let commit = Command::new("git")
        .arg("-C")
        .arg(&main)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("read head");
    let commit = String::from_utf8_lossy(&commit.stdout).trim().to_owned();
    git_ok(&main, &["checkout", "-q", "--detach"]);
    assert_eq!(
        head_branch(main.to_str().unwrap()),
        Some(format!("detached {}", &commit[..7]))
    );
}

/// A built-in is always discovered, so "the module exists" cannot mean "the user owns it". Getting
/// this wrong suppressed every built-in session row for everyone.
#[test]
fn only_a_file_in_the_extension_root_makes_a_builtin_user_owned() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    let catalog = std::sync::Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4, std::sync::Arc::new(|| {}));
    let mut host = ExtensionHost::load(
        directory.path(),
        std::sync::Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );
    assert!(
        !host.is_user_owned("sessions"),
        "the built-in sessions module is not user-owned"
    );

    fs::write(
        directory.path().join("sessions.luau"),
        "bootty.ui.register({ id = \"sessions\", placement = \"sidebar\" }, function()\n\treturn {}\nend)\n",
    )
    .expect("write a sessions override");
    host.refresh(std::time::Instant::now() + Duration::from_secs(1));
    assert!(
        host.is_user_owned("sessions"),
        "an override in the extension root is user-owned"
    );
}

/// The module limit is a runaway backstop, not a budget: overflow must shed the excess and keep a
/// working set, because a refused scan reconciles nothing and freezes every module.
#[test]
fn modules_past_the_limit_are_shed_rather_than_failing_the_whole_scan() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    for index in 0..300 {
        fs::write(
            directory.path().join(format!("m{index:03}.luau")),
            format!(
                "bootty.ui.register({{ id = \"m{index:03}\", placement = \"sidebar\" }}, function()\n\treturn {{}}\nend)\n"
            ),
        )
        .expect("write a module");
    }
    let identities = module_identities(directory.path()).expect("the scan still succeeds");
    // The built-ins are always present on top of whatever the root contributes.
    assert!(
        identities.len() > 256,
        "the built-ins load alongside the shed set"
    );
    assert!(
        identities
            .iter()
            .filter(|identity| identity.as_str().starts_with('m'))
            .count()
            <= 256,
        "no more than the limit is loaded from the root"
    );
}

/// A user override that will not load must not take its built-in down with it, and must say so.
#[test]
fn a_broken_override_falls_back_to_its_builtin() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    fs::write(directory.path().join("clock.luau"), "return {ff\n")
        .expect("write a broken override");
    let catalog = std::sync::Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4, std::sync::Arc::new(|| {}));
    let host = ExtensionHost::load(
        directory.path(),
        std::sync::Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );
    let sources = host.module_sources();
    assert!(
        sources
            .failures
            .iter()
            .any(|(identity, _)| identity.as_str() == "clock.luau"),
        "the broken override is reported, not swallowed"
    );
    assert!(
        catalog
            .surfaces()
            .iter()
            .any(|surface| surface.snapshot.declaration.id == "clock"),
        "the built-in clock still publishes its surface"
    );
}

/// Saving the built-in verbatim must not pin a copy of it, or the module stops picking up updates.
#[test]
fn saving_a_source_equal_to_the_builtin_drops_the_override() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    let catalog = std::sync::Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4, std::sync::Arc::new(|| {}));
    let mut host = ExtensionHost::load(
        directory.path(),
        std::sync::Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );
    let identity = ModuleIdentity::parse("clock.luau").expect("a valid identity");
    let ModuleSourceOutcome::Loaded { source, .. } =
        host.apply_module_source_request(ModuleSourceRequest::Load(identity.clone()))
    else {
        panic!("a load answers with a source");
    };
    let outcome = host.apply_module_source_request(ModuleSourceRequest::Save {
        identity,
        source: source.source,
    });
    assert!(matches!(outcome, ModuleSourceOutcome::Saved(Ok(_))));
    assert!(
        !directory.path().join("clock.luau").exists(),
        "an unchanged source leaves no override behind"
    );
}

/// A built-in declares its surface through the wrapper the host loads it with, so a preview has to
/// use the wrapped source. Previewing the bare file registers nothing and renders an empty mock.
#[test]
fn previewing_a_builtin_publishes_its_surface_with_example_data() {
    let surfaces = preview_builtin_surfaces("sessions", Vec::new()).expect("the preview runs");
    let sessions = surfaces
        .iter()
        .find(|surface| surface.declaration.id == "sessions")
        .expect("the built-in declares its own surface");
    assert!(
        sessions.items.iter().any(|item| item.session_id.is_some()),
        "the preview facts supply example sessions"
    );
}

/// Every built-in has to preview as itself. A module that queries the machine renders nothing
/// unless the preview cache seeds an answer for the command it runs.
#[test]
fn every_builtin_sidebar_module_previews_with_items() {
    for name in ["sessions", "codexbar"] {
        let surfaces = preview_builtin_surfaces(name, Vec::new())
            .unwrap_or_else(|error| panic!("{name} previews: {error}"));
        let surface = surfaces
            .iter()
            .find(|surface| surface.declaration.id == name)
            .unwrap_or_else(|| panic!("{name} declares its own surface"));
        assert!(
            !surface.items.is_empty(),
            "{name} renders something from the example data"
        );
    }
}

/// A module that reads the machine cannot render in a sandbox, so an unedited one previews from
/// what it is publishing right now instead.
#[test]
fn the_live_render_is_available_for_previewing_an_unedited_module() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    let catalog = std::sync::Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4, std::sync::Arc::new(|| {}));
    let host = ExtensionHost::load(
        directory.path(),
        std::sync::Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );
    let sources = host.module_sources();
    let live = sources.live_for("clock.luau");
    assert!(
        live.iter().any(|surface| surface.declaration.id == "clock"),
        "a loaded module offers its live surface for preview"
    );
    assert!(
        sources.live_for("nothing.luau").is_empty(),
        "a module that is not loaded offers nothing"
    );
}
