use std::sync::{Arc, mpsc};

use bootty_app::commands::{
    Caller, CommandCatalog, CommandDescriptor, CommandInvocation, CommandOutcome, CompactSchema,
    MutationClass, ResourceKind,
};

#[test]
fn core_commands_share_one_typed_catalog_contract() {
    let catalog = CommandCatalog::default();
    let read = catalog
        .describe("terminal.read")
        .expect("terminal read command");
    let write = catalog
        .describe("terminal.write")
        .expect("terminal write command");

    assert_eq!(read.mutation, MutationClass::Read);
    assert_eq!(read.target, Some(ResourceKind::Terminal));
    assert_eq!(write.mutation, MutationClass::Write);
    assert_eq!(write.target, Some(ResourceKind::Terminal));
    assert!(matches!(
        catalog.resolve(CommandInvocation::from_action(
            "terminal.write",
            Caller::Socket,
        )),
        Err(CommandOutcome::Failed { code, .. }) if code == "invalid_arguments"
    ));
    assert!(matches!(
        catalog.resolve(CommandInvocation::from_action(
            "missing.command",
            Caller::Socket,
        )),
        Err(CommandOutcome::Failed { code, .. }) if code == "unknown_command"
    ));
}

#[test]
fn extension_commands_are_namespaced_and_cleared_together() {
    let catalog = CommandCatalog::default();
    let descriptor = CommandDescriptor {
        id: "agent.inspect".to_owned(),
        title: "Inspect Agent".to_owned(),
        description: "Inspect one agent session.".to_owned(),
        mutation: MutationClass::Read,
        arguments: CompactSchema::default(),
        target: Some(ResourceKind::Session),
        palette: false,
    };
    let handler = Arc::new(|_, _, _| {
        let (sender, receiver) = mpsc::channel();
        sender
            .send(CommandOutcome::success())
            .expect("send extension outcome");
        receiver
    });

    assert!(
        catalog
            .register_extension("other", descriptor.clone(), handler.clone())
            .is_err()
    );
    catalog
        .register_extension("agent", descriptor, handler)
        .expect("register namespaced extension command");
    assert!(catalog.describe("agent.inspect").is_some());

    catalog.clear_extensions();
    assert!(catalog.describe("agent.inspect").is_none());
}
