use std::collections::BTreeSet;

use bootty_app::commands::{
    Caller, CommandDescriptor, CommandInvocation, CommandOrigin, CommandOutcome, CommandRegistry,
    CommandTargetKind, CompactSchema, CoreCommandExecutor, ExtensionCommandRegistry, MutationClass,
    ResourceKind, ValueType,
};

fn invocation(command: &str, arguments: &[&str]) -> CommandInvocation {
    CommandInvocation {
        command: command.to_owned(),
        arguments: arguments.iter().map(|value| (*value).to_owned()).collect(),
        caller: Caller::Cli,
        target: None,
        confirmation: None,
    }
}

#[test]
fn legacy_actions_and_canonical_commands_share_one_live_registration() {
    let registry = CommandRegistry::core();

    let legacy = registry
        .resolve(CommandInvocation::from_action(
            "split_right",
            Caller::Keybinding,
        ))
        .unwrap();
    let canonical = registry
        .resolve(invocation("pane.split", &["right"]))
        .unwrap();

    assert_eq!(legacy.descriptor.id, "pane.split");
    assert_eq!(canonical.descriptor.id, "pane.split");
    assert_eq!(legacy.executor, canonical.executor);
    assert!(
        registry
            .describe("pane.split")
            .unwrap()
            .aliases
            .iter()
            .any(|alias| alias == "split_right")
    );
}

#[test]
fn canonical_session_selector_accepts_names_while_legacy_alias_requires_an_index() {
    let registry = CommandRegistry::core();

    let canonical = registry
        .resolve(invocation("session.select", &["build"]))
        .unwrap();
    assert_eq!(
        canonical.executor,
        CoreCommandExecutor::SessionSelect {
            selector: "build".to_owned(),
        }
    );

    assert!(matches!(
        registry.resolve(invocation("select_session", &["build"])),
        Err(CommandOutcome::Failed { .. })
    ));
    let legacy = registry
        .resolve(invocation("select_session", &["2"]))
        .unwrap();
    assert_eq!(legacy.descriptor.id, "session.select");
}

#[test]
fn commands_with_required_resources_reject_missing_arguments_before_execution() {
    let registry = CommandRegistry::core();

    for command in [
        "session.create",
        "directory.resolve",
        "directory.usage.list",
        "worktree.list",
        "worktree.get",
        "worktree.create",
        "worktree.remove",
    ] {
        assert!(
            matches!(
                registry.resolve(invocation(command, &[])),
                Err(CommandOutcome::Failed { .. })
            ),
            "{command} accepted missing arguments"
        );
    }
}
#[test]
fn worktree_commands_apply_defaults_before_building_typed_executors() {
    let registry = CommandRegistry::core();

    assert_eq!(
        registry
            .resolve(invocation("worktree.create", &["/repo", "feature"]))
            .unwrap()
            .executor,
        CoreCommandExecutor::WorktreeCreate {
            repository_path: "/repo".to_owned(),
            branch: "feature".to_owned(),
            managed_by_bootty: true,
        }
    );
    assert_eq!(
        registry
            .resolve(invocation("worktree.remove", &["/repo/worktree"]))
            .unwrap()
            .executor,
        CoreCommandExecutor::WorktreeRemove {
            path: "/repo/worktree".to_owned(),
            force: false,
            confirmation: None,
        }
    );
}

#[test]
fn list_reports_each_live_core_registration_once() {
    let descriptors = CommandRegistry::core().list().collect::<Vec<_>>();
    let ids = descriptors
        .iter()
        .map(|descriptor| descriptor.id.as_str())
        .collect::<BTreeSet<_>>();

    assert_eq!(ids.len(), descriptors.len());
    assert!(ids.contains("command.list"));
    assert!(ids.contains("pane.split"));
    assert!(ids.contains("terminal.send_text"));
    assert!(descriptors.iter().all(|descriptor| {
        descriptor.origin == Some(CommandOrigin::Core) && descriptor.availability.is_none()
    }));
    let registry = CommandRegistry::core();
    assert_eq!(
        registry.describe("terminal.search.close").unwrap().mutation,
        MutationClass::Write
    );
    assert_eq!(
        registry.describe("session.create").unwrap().target,
        Some(ResourceKind::Binding)
    );
    assert_eq!(registry.describe("worktree.create").unwrap().target, None);
    assert!(registry.describe("new_window").is_none());
    for (alias, canonical) in [
        ("ping", "system.ping"),
        ("list-commands", "command.list"),
        ("workspace.create", "space.create"),
        ("new-session", "session.create"),
        ("split-window", "pane.split"),
        ("pane.read", "terminal.read"),
        ("events.subscribe", "event.subscribe"),
        ("ui.sidebar.next_session", "session.next"),
    ] {
        assert_eq!(registry.describe(alias).unwrap().id, canonical);
    }
    assert!(
        registry
            .resolve(invocation("terminal.search.next", &[]))
            .is_ok()
    );
    let resize = registry.describe("pane.resize").unwrap();
    assert_eq!(resize.arguments.arguments[0].value_type, ValueType::Object);
    let invoke = registry.describe("command.invoke").unwrap();
    assert!(invoke.arguments.arguments[1].repeated);
    let ping = registry.describe("system.ping").unwrap();
    assert!(
        ping.arguments
            .arguments
            .iter()
            .all(|argument| argument.minimum == Some(1))
    );
    let subscribe = registry.describe("event.subscribe").unwrap();
    assert_eq!(subscribe.targets, vec![CommandTargetKind::Subscription]);
    assert_eq!(
        subscribe.arguments.arguments[2].value_type,
        ValueType::ResourceRef
    );
    assert_eq!(subscribe.arguments.arguments[3].minimum, Some(0));
    assert_eq!(
        registry
            .describe("task.status")
            .unwrap()
            .arguments
            .arguments[0]
            .value_type,
        ValueType::ResourceRef
    );
    assert_eq!(
        registry.describe("directory.resolve").unwrap().mutation,
        MutationClass::Read
    );
    assert_eq!(
        registry.describe("event.snapshot").unwrap().mutation,
        MutationClass::Read
    );
}

#[test]
fn extension_registration_updates_list_describe_and_resolve_atomically() {
    let extensions = ExtensionCommandRegistry::new();
    let registry = CommandRegistry::core().with_extension_registry(extensions);
    let descriptor = CommandDescriptor {
        id: "example.echo".to_owned(),
        title: "Echo".to_owned(),
        description: "Returns extension-owned output".to_owned(),
        aliases: vec!["example.say".to_owned()],
        origin: None,
        mutation: MutationClass::Read,
        arguments: CompactSchema::default(),
        result_schema: None,
        targets: Vec::new(),
        availability: None,
        target: Some(ResourceKind::Extension),
        palette: false,
        palette_metadata: None,
    };

    registry
        .register_extension_command(descriptor, "example", 7)
        .unwrap();

    assert_eq!(registry.describe("example.say").unwrap().id, "example.echo");
    assert!(
        registry
            .list()
            .any(|descriptor| descriptor.id == "example.echo")
    );
    assert_eq!(
        registry
            .resolve(invocation("example.say", &[]))
            .unwrap()
            .executor,
        CoreCommandExecutor::Extension {
            command_id: "example.echo".to_owned(),
            extension_id: "example".to_owned(),
            generation: 7,
        }
    );
    assert_eq!(registry.unregister_extension_commands("example", 7), 1);
    assert!(registry.describe("example.echo").is_none());
}
