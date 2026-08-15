use std::{fs, path::Path, process::Command, time::Duration};

use bootty_command::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, app_command_channel,
};
use bootty_extension::{
    ExtensionCatalog, ExtensionGenerationCandidate, ExtensionGenerationToken, ExtensionHost,
    ModuleColor, ModuleIdentity, ModuleItem, SessionReorder, SurfaceDeclaration, SurfacePlacement,
    SurfaceSnapshot, event_queue, head_branch,
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
