use std::{fs, path::Path, process::Command, time::Duration};

use bootty_command::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, app_command_channel,
};
use bootty_extension::{
    ExtensionCatalog, ExtensionGenerationCandidate, ExtensionGenerationToken, ExtensionHost,
    IntegrationStatus, ModuleColor, ModuleIdentity, ModuleItem, ModuleSourceOutcome,
    ModuleSourceRequest, SessionReorder, SurfaceDeclaration, SurfacePlacement, SurfaceSnapshot,
    event_queue, head_branch, module_identities, preview_builtin_surfaces,
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

/// A module declaring one adapter: a hook script under the integration directory, and a JSON entry
/// in `target` that points at the installed script by absolute path.
fn integration_module(target: &Path, file_path: &str) -> String {
    format!(
        r##"bootty.integration.register({{
    id = "hooks",
    title = "Probe hooks",
    summary = "Reports probe activity to Bootty.",
    files = {{
        {{ path = "{file_path}", contents = "#!/bin/sh\nexit 0\n", executable = true }},
    }},
    merge = {{
        {{
            path = "{target}",
            value = {{ hooks = {{ SessionStart = {{ {{ command = bootty.integration.dir .. "/{file_path}" }} }} }} }},
        }},
    }},
}})
"##,
        target = target.display(),
    )
}

/// A host over `config/extensions`, so the integration directory lands beside it at
/// `config/integrations` the way it does under a real config directory.
fn integration_host(config: &Path, source: &str) -> ExtensionHost {
    let root = config.join("extensions");
    fs::create_dir_all(&root).expect("create the extension root");
    fs::write(root.join("probe.luau"), source).expect("write the probe module");
    // Nothing here invokes an app command, so the receiver may go; the host only needs a sender.
    let (sender, _receiver) = app_command_channel(4, std::sync::Arc::new(|| {}));
    ExtensionHost::load(
        &root,
        std::sync::Arc::new(ExtensionCatalog::default()),
        sender.for_caller(Caller::Luau),
        event_queue().0,
    )
}

fn integration_status(host: &ExtensionHost) -> IntegrationStatus {
    let sources = host.module_sources();
    let integration = sources
        .integrations
        .iter()
        .find(|integration| integration.declaration.module == "probe")
        .expect("the probe module declares an integration");
    integration.status
}

fn install_request(install: bool) -> ModuleSourceRequest {
    install_request_for("hooks", install)
}

fn install_request_for(id: &str, install: bool) -> ModuleSourceRequest {
    let module = "probe".to_owned();
    let id = id.to_owned();
    if install {
        ModuleSourceRequest::InstallIntegration { module, id }
    } else {
        ModuleSourceRequest::UninstallIntegration { module, id }
    }
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(path).expect("read the merge target"))
        .expect("the merge target is JSON")
}

/// The whole point of merging rather than writing: a real `hooks.json` already holds hooks Bootty
/// knows nothing about, and neither installing nor removing ours may touch them.
#[test]
fn installing_an_integration_merges_into_a_file_it_never_owns() {
    let config = tempfile::tempdir().expect("temporary config directory");
    let target = config.path().join("codex").join("hooks.json");
    fs::create_dir_all(target.parent().expect("target parent")).expect("create the target parent");
    fs::write(
        &target,
        r#"{"hooks":{"Stop":[{"command":"/usr/bin/true"}]}}"#,
    )
    .expect("write the pre-existing hooks");

    let mut host = integration_host(config.path(), &integration_module(&target, "probe/hook.sh"));
    assert_eq!(integration_status(&host), IntegrationStatus::Missing);

    let outcome = host.apply_module_source_request(install_request(true));
    assert!(
        matches!(outcome, ModuleSourceOutcome::Integration(Ok(()))),
        "install answers with an outcome: {outcome:?}"
    );
    assert_eq!(integration_status(&host), IntegrationStatus::Installed);

    let script = config.path().join("integrations").join("probe/hook.sh");
    assert!(script.is_file(), "the adapter file is written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&script)
            .expect("script metadata")
            .permissions()
            .mode();
        assert!(mode & 0o111 != 0, "an executable adapter is executable");
    }

    let merged = read_json(&target);
    assert_eq!(
        merged["hooks"]["Stop"],
        serde_json::json!([{"command": "/usr/bin/true"}]),
        "the user's unrelated hook survives the merge"
    );
    assert_eq!(
        merged["hooks"]["SessionStart"],
        serde_json::json!([{"command": script.to_string_lossy()}]),
        "our hook points at the installed script"
    );

    let outcome = host.apply_module_source_request(install_request(false));
    assert!(matches!(outcome, ModuleSourceOutcome::Integration(Ok(()))));
    assert!(!script.exists(), "uninstall removes the file it wrote");
    assert_eq!(integration_status(&host), IntegrationStatus::Missing);
    let remaining = read_json(&target);
    assert_eq!(
        remaining,
        serde_json::json!({"hooks": {"Stop": [{"command": "/usr/bin/true"}]}}),
        "uninstall takes back exactly what it added"
    );
}

/// Installing twice must not append our entry a second time, or every restart would grow the
/// user's config by one more copy of the same hook.
#[test]
fn installing_an_integration_twice_adds_nothing_the_second_time() {
    let config = tempfile::tempdir().expect("temporary config directory");
    let target = config.path().join("hooks.json");
    let mut host = integration_host(config.path(), &integration_module(&target, "probe/hook.sh"));

    assert!(matches!(
        host.apply_module_source_request(install_request(true)),
        ModuleSourceOutcome::Integration(Ok(()))
    ));
    let first = read_json(&target);
    assert!(matches!(
        host.apply_module_source_request(install_request(true)),
        ModuleSourceOutcome::Integration(Ok(()))
    ));
    assert_eq!(read_json(&target), first, "a second install is a no-op");
    assert_eq!(integration_status(&host), IntegrationStatus::Installed);
}

/// A declared path that climbs out of the integration directory is refused when the module loads,
/// so no install can ever write outside it.
#[test]
fn an_integration_file_path_may_not_escape_the_integration_directory() {
    let config = tempfile::tempdir().expect("temporary config directory");
    let target = config.path().join("hooks.json");
    let host = integration_host(config.path(), &integration_module(&target, "../escape.sh"));
    let sources = host.module_sources();
    assert!(
        !sources
            .integrations
            .iter()
            .any(|integration| integration.declaration.module == "probe"),
        "a module with an escaping path declares nothing"
    );
    assert!(
        sources
            .failures
            .iter()
            .any(|(identity, error)| identity.as_str() == "probe.luau"
                && error.contains("stay inside the integration directory")),
        "the module fails to load, with why: {:?}",
        sources.failures
    );
}

/// The dangerous merge is into an array the user already has an entry in — a real `hooks.json`
/// carries an unrelated `Stop` hook, and installing ours must append beside it, not replace it.
/// Uninstall then has to take back exactly ours.
#[test]
fn installing_into_an_array_the_user_already_uses_keeps_their_entry() {
    let config = tempfile::tempdir().expect("temporary config directory");
    let target = config.path().join("hooks.json");
    let theirs = serde_json::json!({
        "hooks": {
            "Stop": [{
                "hooks": [{
                    "type": "command",
                    "command": "~/.local/bin/plannotator",
                    "timeout": 345_600,
                }],
            }],
        },
    });
    fs::write(
        &target,
        serde_json::to_string_pretty(&theirs).expect("their hooks"),
    )
    .expect("write the pre-existing hooks");

    let module = format!(
        r##"local script = bootty.integration.dir .. "/probe/hook.sh"
bootty.integration.register({{
    id = "hooks",
    files = {{ {{ path = "probe/hook.sh", contents = "#!/bin/sh\nexit 0\n", executable = true }} }},
    merge = {{
        {{
            path = "{target}",
            value = {{ hooks = {{ Stop = {{ {{ hooks = {{ {{ type = "command", command = script, timeout = 2 }} }} }} }} }} }},
        }},
    }},
}})
"##,
        target = target.display(),
    );
    let mut host = integration_host(config.path(), &module);
    assert!(matches!(
        host.apply_module_source_request(install_request(true)),
        ModuleSourceOutcome::Integration(Ok(()))
    ));

    let stop = read_json(&target)["hooks"]["Stop"].clone();
    let entries = stop.as_array().expect("Stop is an array");
    assert_eq!(entries.len(), 2, "ours joins theirs: {stop:#}");
    assert_eq!(
        entries[0], theirs["hooks"]["Stop"][0],
        "their entry is untouched and still first"
    );

    assert!(matches!(
        host.apply_module_source_request(install_request(false)),
        ModuleSourceOutcome::Integration(Ok(()))
    ));
    assert_eq!(
        read_json(&target)["hooks"]["Stop"],
        theirs["hooks"]["Stop"],
        "uninstall takes back only ours"
    );
}

/// Grouping is the `sessions` module's job: a project holding more than one session gets a header
/// row, its members indent under it, and the last one closes the tree. The preview facts hold
/// `work/api` and `work/web`, which is exactly that shape.
#[test]
fn the_sessions_module_groups_a_project_and_closes_its_tree() {
    let surfaces = preview_builtin_surfaces("sessions", Vec::new()).expect("the preview runs");
    let sessions = surfaces
        .iter()
        .find(|surface| surface.declaration.id == "sessions")
        .expect("the built-in declares its own surface");
    let rows = sessions
        .items
        .iter()
        .filter(|item| matches!(item.kind.as_deref(), Some("group" | "session")))
        .collect::<Vec<_>>();

    assert_eq!(
        rows.iter()
            .map(|item| (item.text.as_str(), item.tree.as_deref(), item.indent))
            .collect::<Vec<_>>(),
        [
            ("work", Some("none"), Some(0)),
            ("api", Some("middle"), Some(2)),
            ("web", Some("last"), Some(2)),
        ],
        "one header, then its members indented under it"
    );
    assert_eq!(
        rows[1].reorder_anchor.as_deref(),
        rows[0].reorder_anchor.as_deref(),
        "the header drags the whole group, so it shares the first session's anchor"
    );
    assert_eq!(rows[1].number, Some(1));
    assert_eq!(rows[2].number, Some(2));
}

/// Every agent bootty ships needs an adapter installed in the tool it reports from, so every agent
/// module has to declare one. A module whose declaration never registers offers the user no way to
/// install it at all, which is indistinguishable from the agent not working.
#[test]
fn every_builtin_agent_module_declares_an_installable_integration() {
    let config = tempfile::tempdir().expect("temporary config directory");
    let root = config.path().join("extensions");
    fs::create_dir_all(&root).expect("create the extension root");
    let (sender, _receiver) = app_command_channel(4, std::sync::Arc::new(|| {}));
    let host = ExtensionHost::load(
        &root,
        std::sync::Arc::new(ExtensionCatalog::default()),
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );
    let sources = host.module_sources();
    let declared = sources
        .integrations
        .iter()
        .map(|integration| integration.declaration.module.as_str())
        .collect::<Vec<_>>();

    for module in ["agents.claude", "agents.codex", "agents.pi"] {
        assert!(
            declared.contains(&module),
            "{module} declares no integration; declared: {declared:?}"
        );
    }
}

/// An adapter for a tool that loads extensions from a directory of its own has to land in that
/// directory. Writing it only into bootty's own integrations directory installs nothing the tool
/// will ever read, which is what happened to the Pi extension: the file was written, the status
/// said installed, and Pi never saw it.
#[test]
fn a_placed_file_lands_where_the_tool_reads_it_and_is_taken_back_on_uninstall() {
    let config = tempfile::tempdir().expect("temporary config directory");
    let home = tempfile::tempdir().expect("temporary home directory");
    let placed = home
        .path()
        .join("agent")
        .join("extensions")
        .join("probe.ts");
    let module = format!(
        r#"bootty.integration.register({{
    id = "extension",
    title = "Probe extension",
    files = {{
        {{ path = "probe/adapter.ts", contents = "export const bootty = 1;\n" }},
    }},
    place = {{
        {{ path = "{}", file = "probe/adapter.ts" }},
    }},
}})
"#,
        placed.display()
    );

    let mut host = integration_host(config.path(), &module);
    assert_eq!(
        integration_status(&host),
        IntegrationStatus::Missing,
        "the adapter is not in the tool's directory yet"
    );

    let outcome = host.apply_module_source_request(install_request_for("extension", true));
    assert!(
        matches!(outcome, ModuleSourceOutcome::Integration(Ok(()))),
        "install answers with an outcome: {outcome:?}"
    );
    assert_eq!(
        fs::read_to_string(&placed).expect("the placed adapter"),
        "export const bootty = 1;\n",
        "the tool's own directory holds the adapter"
    );
    assert_eq!(integration_status(&host), IntegrationStatus::Installed);

    let outcome = host.apply_module_source_request(install_request_for("extension", false));
    assert!(matches!(outcome, ModuleSourceOutcome::Integration(Ok(()))));
    assert!(!placed.exists(), "uninstall takes back the copy it placed");
}

/// A file the user replaced with their own is theirs, so uninstall leaves it alone rather than
/// deleting whatever happens to sit at that path.
#[test]
fn uninstall_leaves_a_placed_file_the_user_replaced() {
    let config = tempfile::tempdir().expect("temporary config directory");
    let home = tempfile::tempdir().expect("temporary home directory");
    let placed = home.path().join("probe.ts");
    let module = format!(
        r#"bootty.integration.register({{
    id = "extension",
    files = {{
        {{ path = "probe/adapter.ts", contents = "ours\n" }},
    }},
    place = {{
        {{ path = "{}", file = "probe/adapter.ts" }},
    }},
}})
"#,
        placed.display()
    );

    let mut host = integration_host(config.path(), &module);
    host.apply_module_source_request(install_request_for("extension", true));
    fs::write(&placed, "mine\n").expect("replace the adapter");

    host.apply_module_source_request(install_request_for("extension", false));
    assert_eq!(
        fs::read_to_string(&placed).expect("the user's file"),
        "mine\n",
        "uninstall removes only a copy that is still ours"
    );
}

/// The Pi adapter has to reach Pi's own extensions directory; nothing else installs it.
#[test]
fn the_pi_agent_places_its_adapter_in_pis_extension_directory() {
    let config = tempfile::tempdir().expect("temporary config directory");
    let root = config.path().join("extensions");
    fs::create_dir_all(&root).expect("create the extension root");
    let (sender, _receiver) = app_command_channel(4, std::sync::Arc::new(|| {}));
    let host = ExtensionHost::load(
        &root,
        std::sync::Arc::new(ExtensionCatalog::default()),
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );
    let sources = host.module_sources();
    let pi = sources
        .integrations
        .iter()
        .find(|integration| integration.declaration.module == "agents.pi")
        .expect("the pi agent declares an integration");

    assert_eq!(
        pi.declaration
            .place
            .iter()
            .map(|placement| placement.path.as_str())
            .collect::<Vec<_>>(),
        ["~/.pi/agent/extensions/bootty.ts"],
        "the adapter goes where Pi loads extensions from"
    );
}
