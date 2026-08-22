use pretty_assertions::assert_eq;
use rstest::rstest;

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use assert_fs::prelude::*;
use bootty_app::commands::{CommandCatalog, CommandExecutor};
use bootty_app::{
    AppState, ModalDialog,
    ui::{
        ditch::{DitchAction, DitchSessionEvent},
        new_session_picker::NewSessionPickerEvent,
    },
};
use bootty_command::{
    AppCommandReceiver, AppCommandRequest, AppCommandSender, Caller, CommandCancellation,
    CommandInvocation, CommandOutcome, CommandTarget, MutationClass, ResourceKind, ValueType,
    app_command_channel as command_channel,
};
use bootty_config::config::MultiplexerBackendConfig;
use bootty_extension::{ExtensionHost, event_queue};
use bootty_workspace::WorkspaceRepository;
use rusqlite::Connection;

#[path = "support/idle_frames.rs"]
mod frames;
mod support;
#[path = "support/config.rs"]
mod test_config;

fn app_command_channel(capacity: usize) -> (AppCommandSender, AppCommandReceiver) {
    command_channel(capacity, Arc::new(|| {}))
}

fn load_extension(
    source: &str,
) -> (
    assert_fs::TempDir,
    Arc<CommandCatalog>,
    AppCommandReceiver,
    ExtensionHost,
) {
    let directory = assert_fs::TempDir::new().expect("temporary extensions");
    directory
        .child("probe.luau")
        .write_str(source)
        .expect("write extension source");
    let catalog = Arc::new(CommandCatalog::default());
    let (sender, receiver) = app_command_channel(4);
    let host = ExtensionHost::load(
        directory.path(),
        catalog.extensions_arc(),
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );
    (directory, catalog, receiver, host)
}

fn native_state(directory: &Path) -> AppState {
    let config = test_config::config(
        directory.join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    AppState::new(config, support::backends(), Arc::new(|| {}), None, None).expect("app state")
}

fn open_native_session(state: &mut AppState, cwd: &Path, started: Instant) {
    let outcome = submit_action(state, "new_mux_session", Caller::Socket, started);
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert!(matches!(
        state.modal_dialog(),
        Some(ModalDialog::NewSession(_))
    ));
    state.apply_picker_event(NewSessionPickerEvent::CreateSession {
        cwd: cwd.to_string_lossy().into_owned(),
    });
    for tick in 1..5 {
        state.update_frame(frames::idle_frame(started + Duration::from_millis(tick)));
    }
}

fn pane_count(state: &AppState) -> usize {
    state
        .pane_rects(
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(200.0, 100.0)),
            4.0,
        )
        .len()
}

#[rstest]
#[case("terminal.read", MutationClass::Read, Some(ResourceKind::Terminal))]
#[case("terminal.write", MutationClass::Write, Some(ResourceKind::Terminal))]
#[case("terminal.paste", MutationClass::Write, Some(ResourceKind::Terminal))]
#[case("terminal.submit", MutationClass::Write, Some(ResourceKind::Terminal))]
#[case("resource.current", MutationClass::Read, None)]
fn core_commands_publish_typed_descriptors(
    #[case] id: &str,
    #[case] mutation: MutationClass,
    #[case] target: Option<ResourceKind>,
) {
    let catalog = CommandCatalog::default();
    let descriptor = catalog.describe(id).expect("core command descriptor");
    assert_eq!((descriptor.mutation, descriptor.target), (mutation, target));
}

#[test]
fn core_command_resolution_reports_executor_and_argument_boundaries() {
    let catalog = CommandCatalog::default();
    assert!(matches!(
        catalog
            .resolve(CommandInvocation::from_action(
                "terminal.read",
                Caller::Socket,
            ))
            .expect("resolve core command")
            .executor,
        CommandExecutor::Core(_)
    ));
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
fn command_specs_keep_presentation_policy_and_arguments_together() {
    let catalog = CommandCatalog::default();

    let appearance = catalog
        .describe("change_appearance")
        .expect("appearance command");
    assert_eq!(appearance.title, "Change Appearance");
    assert_eq!(appearance.mutation, MutationClass::Write);
    assert_eq!(appearance.target, Some(ResourceKind::ApplicationWindow));
    let [appearance_argument] = appearance.arguments.arguments.as_slice() else {
        panic!("appearance argument schema: {:?}", appearance.arguments);
    };
    assert_eq!(appearance_argument.name, "appearance");
    assert_eq!(appearance_argument.value_type, ValueType::String);
    assert!(appearance_argument.required);
    assert_eq!(appearance_argument.choices, ["system", "light", "dark"]);

    let clipboard = catalog
        .describe("copy_to_clipboard")
        .expect("clipboard command");
    let [clipboard_argument] = clipboard.arguments.arguments.as_slice() else {
        panic!("clipboard argument schema: {:?}", clipboard.arguments);
    };
    assert!(!clipboard_argument.required);
    assert_eq!(clipboard_argument.choices, ["plain", "vt", "html", "mixed"]);

    assert!(catalog.describe("move_tab").expect("move tab").palette);
    assert!(!catalog.describe("select_tab").expect("select tab").palette);
    assert_eq!(
        catalog
            .describe("close_surface")
            .expect("close pane")
            .mutation,
        MutationClass::Destructive
    );
    assert!(matches!(
        catalog.resolve(CommandInvocation::from_action(
            "change_appearance:sepia",
            Caller::Socket,
        )),
        Err(CommandOutcome::Failed { code, .. }) if code == "invalid_arguments"
    ));
    assert!(matches!(
        catalog.resolve(CommandInvocation::from_action("select_tab:0", Caller::Socket)),
        Err(CommandOutcome::Failed { code, .. }) if code == "invalid_arguments"
    ));
    assert!(
        catalog
            .resolve(CommandInvocation::from_action(
                "copy_to_clipboard",
                Caller::Socket,
            ))
            .is_ok()
    );
}

#[test]
fn extension_command_cannot_shadow_core_resolution() {
    let (_directory, catalog, _receiver, _host) = load_extension(
        r#"
bootty.commands.register({
    id = "terminal.read",
    title = "Shadowed Terminal Read",
    description = "An extension replacement.",
    mutation = "destructive",
    target = "pane",
}, function() return { shadowed = true } end)
"#,
    );
    let core_descriptor = catalog
        .describe("terminal.read")
        .expect("core terminal read command");

    assert_eq!(
        catalog
            .list()
            .iter()
            .filter(|descriptor| descriptor.id == "terminal.read")
            .count(),
        1
    );
    assert_eq!(catalog.extensions().describe("terminal.read"), None);
    assert_eq!(
        catalog.describe("terminal.read"),
        Some(core_descriptor.clone())
    );
    let resolved = catalog
        .resolve(CommandInvocation::from_action(
            "terminal.read",
            Caller::Socket,
        ))
        .expect("resolve core command");
    assert_eq!(resolved.descriptor, core_descriptor);
    assert!(matches!(resolved.executor, CommandExecutor::Core(_)));
}

#[test]
fn discovered_resource_target_cannot_retarget_a_replacement_binding() {
    let directory = assert_fs::TempDir::new().expect("temporary workspace");
    let config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    let started = Instant::now();
    let mut first = AppState::new(
        config.clone(),
        support::backends(),
        Arc::new(|| {}),
        None,
        None,
    )
    .expect("first app state");
    let current = submit_command(
        &mut first,
        CommandInvocation::new(
            "resource.current",
            vec!["binding".to_owned()],
            Caller::Socket,
        ),
        started,
    );
    let CommandOutcome::Success { value, .. } = current else {
        panic!("current binding outcome: {current:?}");
    };
    let target: CommandTarget =
        serde_json::from_value(value["target"].clone()).expect("current binding target");
    drop(first);

    let mut replacement = AppState::new(config, support::backends(), Arc::new(|| {}), None, None)
        .expect("replacement app state");
    let mut invocation = CommandInvocation::new("edit_space", Vec::new(), Caller::Socket);
    invocation.target = Some(target);
    let outcome = submit_command(
        &mut replacement,
        invocation,
        started + Duration::from_millis(20),
    );
    assert!(matches!(outcome, CommandOutcome::StaleTarget { .. }));
}

#[test]
fn native_split_command_publishes_the_binding_owned_layout() {
    let directory = assert_fs::TempDir::new().expect("temporary workspace");
    let started = Instant::now();
    let mut state = native_state(directory.path());
    open_native_session(&mut state, directory.path(), started);

    let outcome = submit_action(
        &mut state,
        "split_right",
        Caller::Socket,
        started + Duration::from_millis(10),
    );

    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert!(state.native_multi_pane());
    assert_eq!(pane_count(&state), 2);
    assert!(state.focused_pane().is_some());

    let direct = submit_action(
        &mut state,
        "split_right",
        Caller::Keybinding,
        started + Duration::from_millis(15),
    );
    assert!(
        matches!(direct, CommandOutcome::Success { .. }),
        "{direct:?}"
    );
    assert_eq!(pane_count(&state), 3);

    let unsupported = submit_action(
        &mut state,
        "toggle_pane_zoom",
        Caller::Socket,
        started + Duration::from_millis(20),
    );
    assert!(matches!(unsupported, CommandOutcome::Unsupported { .. }));
    assert_eq!(pane_count(&state), 3);
}

#[test]
fn ditch_session_commits_membership_after_authoritative_command() {
    let directory = assert_fs::TempDir::new().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let started = Instant::now();
    let mut state = native_state(directory.path());
    let created = submit_action(&mut state, "new_tab", Caller::Socket, started);
    assert!(
        matches!(created, CommandOutcome::Success { .. }),
        "{created:?}"
    );

    let target = (0..250)
        .find_map(|tick| {
            state.update_frame(frames::idle_frame(
                started + Duration::from_millis(250 + tick),
            ));
            std::thread::sleep(Duration::from_millis(1));
            state
                .binding_session_groups()
                .into_iter()
                .find_map(|group| group.sessions.first().map(|session| group.target(session)))
        })
        .expect("native session becomes available");
    let original_name = state
        .binding_session_groups()
        .iter()
        .flat_map(|group| group.sessions.iter())
        .find(|session| session.id == target.session_id)
        .expect("live session")
        .name
        .clone();

    assert!(state.open_ditch_session_dialog_for(&target.session_id));
    assert!(matches!(
        state.modal_dialog(),
        Some(ModalDialog::DitchSession(_))
    ));
    state.apply_ditch_session_event(DitchSessionEvent::Ditch {
        session_id: target.session_id.clone(),
        cwd: None,
        action: DitchAction::KillOnly,
    });

    let removed = (0..250).any(|tick| {
        state.update_frame(frames::idle_frame(
            started + Duration::from_millis(500 + tick),
        ));
        std::thread::sleep(Duration::from_millis(1));
        !state
            .binding_session_groups()
            .iter()
            .flat_map(|group| group.sessions.iter())
            .any(|session| session.id == target.session_id)
    });
    assert!(
        removed,
        "authoritative ditch result must remove the live session"
    );

    drop(state);
    let (_, reopened) = WorkspaceRepository::open(&config_path).expect("reopen workspace");
    assert!(
        !reopened.spaces()[0]
            .binding()
            .sessions()
            .backend_names()
            .contains(&original_name)
    );
}

#[test]
fn ditch_submits_after_worktree_removal_when_branch_deletion_fails() {
    let (_repository, main, worktree, duplicate) = repo_with_duplicate_branch();
    let directory = assert_fs::TempDir::new().expect("temporary workspace");
    let started = Instant::now();
    let mut state = native_state(directory.path());
    open_native_session(&mut state, &worktree, started);
    let session_id = state.mux().sessions()[0].id.clone();

    let cwd = worktree.to_string_lossy().into_owned();
    let action = DitchAction::RemoveWorktreeAndBranch {
        force: true,
        branch: "feature".to_owned(),
        repo: main.to_string_lossy().into_owned(),
    };
    let ditch_event = || DitchSessionEvent::Ditch {
        session_id: session_id.clone(),
        cwd: Some(cwd.clone()),
        action: action.clone(),
    };
    let database = directory.path().join("session-order.sqlite3");
    let lock = Connection::open(&database).expect("open lock connection");
    lock.execute_batch("BEGIN IMMEDIATE")
        .expect("hold workspace write lock");
    assert!(state.open_ditch_session_dialog_for(&session_id));
    state.apply_ditch_session_event(ditch_event());
    assert!(
        worktree.exists(),
        "failed Ditch preparation must not remove the worktree"
    );
    assert!(matches!(
        state.modal_dialog(),
        Some(ModalDialog::DitchSession(_))
    ));
    lock.execute_batch("ROLLBACK")
        .expect("release workspace write lock");
    drop(lock);

    state.apply_ditch_session_event(ditch_event());

    assert!(!worktree.exists(), "ditch must remove the linked worktree");
    assert!(
        state
            .last_error()
            .is_some_and(|warning| warning.contains("branch 'feature' remains")),
        "partial cleanup warning must name the remaining branch"
    );
    assert!(
        (0..250).any(|tick| {
            state.update_frame(frames::idle_frame(
                started + Duration::from_millis(10 + tick),
            ));
            std::thread::sleep(Duration::from_millis(1));
            !state
                .binding_session_groups()
                .iter()
                .flat_map(|group| group.sessions.iter())
                .any(|session| session.id == session_id)
        }),
        "partial cleanup must still submit Ditch"
    );
    assert!(duplicate.exists(), "duplicate branch checkout must remain");
    assert!(git_read(&main, &["branch", "--list", "feature"]).contains("feature"));
}

fn repo_with_duplicate_branch() -> (assert_fs::TempDir, PathBuf, PathBuf, PathBuf) {
    let root = assert_fs::TempDir::new().expect("temporary repository root");
    let main = root.path().join("main");
    let worktree = root.path().join("worktree");
    let duplicate = root.path().join("duplicate");
    fs::create_dir(&main).expect("create main worktree");
    git_ok(&main, &["init", "-q", "-b", "main"]);
    git_ok(&main, &["config", "user.email", "test@bootty.dev"]);
    git_ok(&main, &["config", "user.name", "Bootty Test"]);
    fs::write(main.join("README"), "hello").expect("write initial file");
    git_ok(&main, &["add", "."]);
    git_ok(&main, &["commit", "-q", "-m", "init"]);
    git_ok(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            worktree.to_str().expect("UTF-8 worktree path"),
        ],
    );
    git_ok(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "--force",
            duplicate.to_str().expect("UTF-8 duplicate path"),
            "feature",
        ],
    );
    (root, main, worktree, duplicate)
}

fn git_ok(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_read(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[test]
fn native_window_actions_use_the_binding_owned_plan() {
    let directory = assert_fs::TempDir::new().expect("temporary workspace");
    let started = Instant::now();
    let mut state = native_state(directory.path());
    open_native_session(&mut state, directory.path(), started);

    let session = state.mux().sessions()[0].clone();
    let first_window = session.windows[0].id.clone();
    let outcome = submit_action(
        &mut state,
        "new_tab",
        Caller::Keybinding,
        started + Duration::from_millis(5),
    );
    let CommandOutcome::Success { value, .. } = outcome else {
        panic!("new tab outcome: {outcome:?}");
    };
    let created: CommandTarget =
        serde_json::from_value(value["created"].clone()).expect("created terminal target");
    assert_eq!(created.kind, ResourceKind::Terminal);
    for tick in 5..10 {
        state.update_frame(frames::idle_frame(started + Duration::from_millis(tick)));
    }

    let session = state
        .mux()
        .sessions()
        .iter()
        .find(|candidate| candidate.id == session.id)
        .expect("created session");
    assert_eq!(session.windows.len(), 2);
    let second_window = session
        .windows
        .iter()
        .find(|window| window.id != first_window)
        .expect("new window")
        .id
        .clone();
    let outcome = submit_action(
        &mut state,
        "next_tab",
        Caller::Keybinding,
        started + Duration::from_millis(10),
    );
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(state.mux().selected_window(), Some(first_window.as_str()));
    let current = submit_command(
        &mut state,
        CommandInvocation::new(
            "resource.current",
            vec!["terminal".to_owned()],
            Caller::Socket,
        ),
        started + Duration::from_millis(10),
    );
    let CommandOutcome::Success { value, .. } = current else {
        panic!("current terminal outcome: {current:?}");
    };
    let first_terminal: CommandTarget =
        serde_json::from_value(value["target"].clone()).expect("first terminal target");
    let outcome = submit_action(
        &mut state,
        "last_tab",
        Caller::Keybinding,
        started + Duration::from_millis(11),
    );
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(state.mux().selected_window(), Some(second_window.as_str()));

    let mut write = CommandInvocation::new("terminal.write", vec![" ".to_owned()], Caller::Socket);
    write.target = Some(first_terminal);
    let outcome = submit_command(&mut state, write, started + Duration::from_millis(12));
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(state.mux().selected_window(), Some(first_window.as_str()));
}

#[test]
fn extension_commands_are_namespaced_and_removed_by_generation() {
    let (directory, catalog, _receiver, mut host) = load_extension(
        r#"
bootty.commands.register({
    id = "probe.inspect",
    title = "Inspect Probe",
    description = "Inspect one session.",
    target = "session",
}, function() return { ok = true } end)
"#,
    );
    assert!(catalog.describe("probe.inspect").is_some());
    assert!(matches!(
        catalog
            .resolve(CommandInvocation::from_action(
                "probe.inspect",
                Caller::Socket,
            ))
            .expect("resolve extension command")
            .executor,
        CommandExecutor::Extension(_)
    ));

    std::fs::remove_file(directory.path().join("probe.luau")).expect("remove extension source");
    host.refresh(Instant::now() + Duration::from_secs(1));
    host.refresh(Instant::now() + Duration::from_secs(2));
    assert!(catalog.describe("probe.inspect").is_none());
}

#[test]
fn destructive_policy_is_identical_for_core_and_extension_commands() {
    let directory = assert_fs::TempDir::new().expect("temporary workspace");
    let extensions = directory.child("extensions");
    extensions.create_dir_all().expect("extension directory");
    extensions
        .child("test.luau")
        .write_str(
            r#"
bootty.commands.register({
    id = "test.destroy",
    title = "Destroy Test",
    mutation = "destructive",
}, function() return { ok = true } end)
"#,
        )
        .expect("write extension source");
    let config = test_config::default_config(directory.path().join("config.toml"));
    let mut state =
        AppState::new(config, support::backends(), Arc::new(|| {}), None, None).expect("app state");
    let _host = ExtensionHost::load(
        &directory.path().join("extensions"),
        state.command_catalog().extensions_arc(),
        state.app_command_sender(Caller::Luau),
        event_queue().0,
    );
    assert_eq!(
        state
            .command_catalog()
            .describe("test.destroy")
            .map(|command| command.mutation),
        Some(MutationClass::Destructive)
    );

    for (caller, expected_confirmation) in [
        (Caller::CommandPalette, false),
        (Caller::Keybinding, false),
        (Caller::Cli, true),
        (Caller::Socket, true),
        (Caller::Luau, true),
    ] {
        let commands = state.app_command_sender(caller);
        let (response, outcomes) = mpsc::channel();
        let deadline = Instant::now() + Duration::from_secs(1);
        commands
            .try_send(AppCommandRequest {
                invocation: CommandInvocation::from_action("test.destroy", caller),
                deadline,
                cancellation: CommandCancellation::new(),
                response,
            })
            .expect("submit extension command");
        let outcome = loop {
            state.update_frame(frames::idle_frame(Instant::now()));
            match outcomes.try_recv() {
                Ok(outcome) => break outcome,
                Err(mpsc::TryRecvError::Empty) if Instant::now() < deadline => {
                    std::thread::yield_now();
                }
                Err(mpsc::TryRecvError::Empty) => panic!("extension command outcome timed out"),
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("extension command response channel closed")
                }
            }
        };
        assert_eq!(
            matches!(outcome, CommandOutcome::ConfirmationRequired { .. }),
            expected_confirmation,
            "caller {caller:?}"
        );
    }
}

#[test]
fn luau_nested_commands_keep_typed_fields_deadline_and_cancellation() {
    let (_directory, catalog, receiver, _host) = load_extension(
        r#"
bootty.commands.register({
    id = "probe.forward",
    title = "Forward",
    arguments = {{ name = "command", type = "string", required = true }},
}, function(context)
    local target = { kind = "terminal", handle = "opaque", generation = "7" }
    return bootty.commands.invoke({
        command = context.arguments[1],
        arguments = { "payload" },
        target = target,
        confirmation = {
            command = context.arguments[1],
            arguments = { "payload" },
            target = target,
        },
    })
end)
"#,
    );
    let resolved = catalog
        .resolve(CommandInvocation::from_action(
            "probe.forward:ignore",
            Caller::Socket,
        ))
        .expect("resolve extension command");
    let CommandExecutor::Extension(handler) = resolved.executor else {
        panic!("extension executor");
    };
    let deadline = Instant::now() + Duration::from_secs(1);
    let cancellation = CommandCancellation::new();
    let outcome = handler.invoke(resolved.invocation, deadline, cancellation.clone());
    let nested = (0..100)
        .find_map(|_| {
            let request = receiver.try_recv().ok();
            if request.is_none() {
                std::thread::sleep(Duration::from_millis(1));
            }
            request
        })
        .expect("nested app command");

    assert_eq!(nested.invocation.command, "ignore");
    assert_eq!(nested.invocation.arguments, ["payload"]);
    assert_eq!(nested.invocation.caller, Caller::Luau);
    assert_eq!(nested.deadline, deadline);
    assert_eq!(
        nested.invocation.target.as_ref().map(|target| (
            target.kind,
            target.handle.as_str(),
            target.generation,
        )),
        Some((ResourceKind::Terminal, "opaque", 7))
    );
    assert_eq!(
        nested.invocation.confirmation,
        Some(nested.invocation.confirmation())
    );
    nested
        .response
        .send(CommandOutcome::Success {
            value: serde_json::json!({"source": "app"}),
            warnings: Vec::new(),
        })
        .expect("complete nested command");
    let outer = outcome
        .recv_timeout(Duration::from_secs(1))
        .expect("extension outcome");
    assert!(matches!(
        outer,
        CommandOutcome::Success { value, .. }
            if value["status"] == "success" && value["value"]["source"] == "app"
    ));

    let resolved = catalog
        .resolve(CommandInvocation::from_action(
            "probe.forward:ignore",
            Caller::Socket,
        ))
        .expect("resolve extension command again");
    let CommandExecutor::Extension(handler) = resolved.executor else {
        panic!("extension executor");
    };
    let cancellation = CommandCancellation::new();
    let outcome = handler.invoke(
        resolved.invocation,
        Instant::now() + Duration::from_secs(1),
        cancellation.clone(),
    );
    let nested = (0..100)
        .find_map(|_| {
            let request = receiver.try_recv().ok();
            if request.is_none() {
                std::thread::sleep(Duration::from_millis(1));
            }
            request
        })
        .expect("nested cancellable command");
    cancellation.cancel();
    assert!((0..100).any(|_| {
        if nested.cancellation.is_cancelled() {
            true
        } else {
            std::thread::sleep(Duration::from_millis(1));
            false
        }
    }));
    let outer = outcome
        .recv_timeout(Duration::from_secs(1))
        .expect("cancelled extension outcome");
    assert!(matches!(
        outer,
        CommandOutcome::Failed { code, .. } if code == "cancelled"
    ));
}

fn submit_command(
    state: &mut AppState,
    invocation: CommandInvocation,
    started: Instant,
) -> CommandOutcome {
    let commands = state.app_command_sender(Caller::Socket);
    let (response, outcomes) = mpsc::channel();
    commands
        .try_send(AppCommandRequest {
            invocation,
            deadline: started + Duration::from_secs(1),
            cancellation: CommandCancellation::new(),
            response,
        })
        .expect("submit command");
    (0..100)
        .find_map(|tick| {
            state.update_frame(frames::idle_frame(started + Duration::from_millis(tick)));
            outcomes.try_recv().ok()
        })
        .expect("command outcome")
}

fn submit_action(
    state: &mut AppState,
    action: &str,
    caller: Caller,
    started: Instant,
) -> CommandOutcome {
    submit_command(
        state,
        CommandInvocation::from_action(action, caller),
        started,
    )
}
