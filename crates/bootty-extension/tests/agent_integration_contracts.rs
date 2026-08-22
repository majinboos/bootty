#![cfg(unix)]

use std::{
    env, fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

use bootty_command::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, app_command_channel,
};
use bootty_extension::{ExtensionCatalog, ExtensionHost, event_queue};
use serde_json::{Value, json};

/// Wall-clock budget for every step that waits on a real agent process.
///
/// A loaded machine can starve these fixtures for seconds, so the budget only
/// has to be generous enough to never expire on a healthy run. A dead fixture
/// is caught by the `stopped` status instead of by the clock.
const AGENT_BUDGET: Duration = Duration::from_secs(30);

#[test]
fn pi_and_codex_keep_their_native_jsonl_protocols_outside_rust_core() {
    let directory = tempfile::tempdir().expect("temporary agent integration root");
    let pi = executable(
        directory.path(),
        "fake-pi",
        r#"#!/bin/sh
while IFS= read -r line; do
    case "$line" in
        *\"get_state\"*)
            printf '%s\n' '{"id":"bootty-1","type":"response","command":"get_state","success":true,"data":{"sessionId":"pi-session","sessionName":"Pi fixture","isStreaming":false}}'
            ;;
        *\"prompt\"*)
            printf '%s\n' '{"id":"bootty-2","type":"response","command":"prompt","success":true}'
            printf '%s\n' '{"type":"agent_start"}'
            printf '%s\n' '{"type":"agent_settled"}'
            ;;
        *\"abort\"*)
            printf '%s\n' '{"id":"bootty-3","type":"response","command":"abort","success":true}'
            printf '%s\n' '{"type":"agent_settled"}'
            ;;
    esac
done
"#,
    );
    let codex = executable(
        directory.path(),
        "fake-codex",
        r#"#!/bin/sh
while IFS= read -r line; do
    case "$line" in
        *\"method\":\"initialize\"*)
            printf '%s\n' '{"id":0,"result":{"userAgent":"fixture"}}'
            ;;
        *\"thread/start\"*)
            printf '%s\n' '{"id":1,"result":{"thread":{"id":"codex-thread"}}}'
            printf '%s\n' '{"method":"thread/started","params":{"thread":{"id":"codex-thread"}}}'
            ;;
        *\"turn/start\"*)
            printf '%s\n' '{"id":2,"result":{}}'
            printf '%s\n' '{"method":"turn/started","params":{"threadId":"codex-thread","turn":{"id":"codex-turn"}}}'
            printf '%s\n' '{"method":"item/completed","params":{"threadId":"codex-thread","turnId":"codex-turn","item":{"id":"item-7","type":"agentMessage"}}}'
            ;;
        *\"turn/interrupt\"*)
            printf '%s\n' '{"id":3,"result":{}}'
            printf '%s\n' '{"method":"turn/completed","params":{"threadId":"codex-thread","turn":{"id":"codex-turn","status":"interrupted"}}}'
            ;;
    esac
done
"#,
    );

    let catalog = Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4, Arc::new(|| {}));
    let _host = ExtensionHost::load(
        &directory.path().join("extensions"),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );

    assert_failure_contains(
        invoke(
            &catalog,
            "agents.pi.start",
            vec![
                path_text(directory.path()),
                path_text(&directory.path().join("missing-pi")),
            ],
        ),
        "No such file",
    );
    assert_success(invoke(
        &catalog,
        "agents.pi.start",
        vec![path_text(directory.path()), path_text(&pi)],
    ));
    let pi_state = wait_for_state(&catalog, "agents.pi.state", |state| {
        state["session_id"] == json!("pi-session")
    });
    assert_eq!(pi_state["source"], json!("managed"));
    assert_eq!(pi_state["status"], json!("idle"));
    assert_success(invoke(
        &catalog,
        "agents.pi.prompt",
        vec!["inspect the fixture".to_owned()],
    ));
    let pi_state = wait_for_state(&catalog, "agents.pi.state", |state| {
        state["last_event"] == json!("agent_settled")
    });
    assert_eq!(pi_state["status"], json!("idle"));
    assert_success(invoke_confirmed(&catalog, "agents.pi.abort", Vec::new()));

    assert_success(invoke(
        &catalog,
        "agents.codex.start",
        vec![path_text(directory.path()), path_text(&codex)],
    ));
    let codex_state = wait_for_state(&catalog, "agents.codex.state", |state| {
        state["thread_id"] == json!("codex-thread")
    });
    assert_eq!(codex_state["source"], json!("managed"));
    assert_eq!(codex_state["status"], json!("idle"));
    assert_success(invoke(
        &catalog,
        "agents.codex.prompt",
        vec!["inspect the fixture".to_owned()],
    ));
    let codex_state = wait_for_state(&catalog, "agents.codex.state", |state| {
        state["turn_id"] == json!("codex-turn") && state["status"] == json!("working")
    });
    assert_eq!(codex_state["thread_id"], json!("codex-thread"));
    assert_eq!(codex_state["turn_id"], json!("codex-turn"));
    assert_eq!(codex_state["status"], json!("working"));
    assert_success(invoke_confirmed(
        &catalog,
        "agents.codex.interrupt",
        Vec::new(),
    ));
    let codex_state = wait_for_state(&catalog, "agents.codex.state", |state| {
        state["last_event"] == json!("turn/completed")
    });
    assert_eq!(codex_state["status"], json!("interrupted"));
}

#[test]
fn reported_agent_events_preserve_opaque_identities_per_pane() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    let catalog = Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4, Arc::new(|| {}));
    let _host = ExtensionHost::load(
        directory.path(),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );

    let pi = success_value(invoke(
        &catalog,
        "agents.pi.ingest",
        vec![
            json!({
                "type": "tool_execution_start",
                "sessionId": "pi/session:7",
                "toolCallId": "tool#opaque",
                "toolName": "read"
            })
            .to_string(),
            "%1".to_owned(),
        ],
    ));
    assert_eq!(pi["source"], json!("existing"));
    assert_eq!(pi["session_id"], json!("pi/session:7"));
    assert_eq!(pi["status"], json!("tool:read"));

    let codex = success_value(invoke(
        &catalog,
        "agents.codex.ingest",
        vec![
            json!({
                "hook_event_name": "PreToolUse",
                "session_id": "thread/opaque",
                "turn_id": "turn#9",
                "tool_name": "Bash",
                "transcript_path": "/must/not/be/read"
            })
            .to_string(),
            "%2".to_owned(),
        ],
    ));
    assert_eq!(codex["source"], json!("existing"));
    assert_eq!(codex["thread_id"], json!("thread/opaque"));
    assert_eq!(codex["turn_id"], json!("turn#9"));
    assert_eq!(codex["status"], json!("tool:Bash"));

    let claude = success_value(invoke(
        &catalog,
        "agents.claude.ingest",
        vec![
            json!({
                "hook_event_name": "PreToolUse",
                "session_id": "claude/opaque",
                "tool_name": "Edit",
                "transcript_path": "/must/not/be/read"
            })
            .to_string(),
            "%3".to_owned(),
        ],
    ));
    assert_eq!(claude["source"], json!("existing"));
    assert_eq!(claude["session_id"], json!("claude/opaque"));
    assert_eq!(claude["status"], json!("tool:Edit"));

    // Each pane keeps its own state, and the managed slot a pane never reported into stays empty.
    assert_eq!(
        success_value(invoke(
            &catalog,
            "agents.codex.state",
            vec!["%2".to_owned()]
        ))["status"],
        json!("tool:Bash")
    );
    assert_eq!(
        success_value(invoke(&catalog, "agents.codex.state", Vec::new()))["source"],
        json!("none")
    );

    // A session that ends leaves nothing behind for its pane.
    let ended = success_value(invoke(
        &catalog,
        "agents.claude.ingest",
        vec![
            json!({ "hook_event_name": "SessionEnd", "session_id": "claude/opaque" }).to_string(),
            "%3".to_owned(),
        ],
    ));
    assert_eq!(ended["status"], json!("stopped"));
    assert_eq!(
        success_value(invoke(
            &catalog,
            "agents.claude.state",
            vec!["%3".to_owned()]
        ))["source"],
        json!("none")
    );
}

/// Every agent module ships the adapter the other tool needs, and `integrations/` on disk is that
/// same text — the docs point people at those paths.
#[test]
fn every_agent_module_declares_the_adapter_it_reads_events_from() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    let catalog = Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4, Arc::new(|| {}));
    let host = ExtensionHost::load(
        directory.path(),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );
    let sources = host.module_sources();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

    for module in ["agents.pi", "agents.codex", "agents.claude"] {
        let declared = sources
            .integrations
            .iter()
            .filter(|state| state.declaration.module == module)
            .collect::<Vec<_>>();
        assert_eq!(declared.len(), 1, "{module} declares one adapter");
        let files = &declared[0].declaration.files;
        assert!(!files.is_empty(), "{module} declares adapter files");
        for file in files {
            assert_eq!(
                fs::read_to_string(repository.join("integrations").join(&file.path))
                    .unwrap_or_else(|_| panic!("integrations/{} is missing", file.path)),
                file.contents,
                "integrations/{} drifted from the module that declares it",
                file.path
            );
        }
    }
}

#[test]
fn agent_hooks_forward_native_json_with_the_reporting_pane() {
    let directory = tempfile::tempdir().expect("temporary Codex hook root");
    let arguments = directory.path().join("arguments");
    executable(
        directory.path(),
        "bootty",
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$BOOTTY_HOOK_ARGUMENTS\"\n",
    );
    let path = env::var_os("PATH").unwrap_or_default();
    let path = env::join_paths(
        std::iter::once(directory.path().to_path_buf()).chain(env::split_paths(&path)),
    )
    .expect("fixture PATH");

    for (agent, command) in [
        ("codex", "agents.codex.ingest"),
        ("claude", "agents.claude.ingest"),
    ] {
        let hook = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../integrations/{agent}/bootty-hook.sh"));
        let mut child = Command::new("/bin/sh")
            .arg(&hook)
            .env("BOOTTY_HOOK_ARGUMENTS", &arguments)
            .env("PATH", &path)
            // tmux names the pane the hook ran in; a pane bootty spawned itself is named by
            // `BOOTTY_PANE`, which the hook only falls back to.
            .env("TMUX_PANE", "%4")
            .env("BOOTTY_PANE", "pane-1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("start hook");
        child
            .stdin
            .take()
            .expect("hook stdin")
            .write_all(br#"{"hook_event_name":"Stop","session_id":"opaque/thread"}"#)
            .expect("write native hook event");
        let output = child.wait_with_output().expect("finish hook");

        assert!(output.status.success());
        assert_eq!(output.stdout, b"{}\n");
        assert_eq!(
            fs::read_to_string(&arguments).expect("captured Bootty arguments"),
            format!(
                "--json\ncommand\n{command}\n{{\"hook_event_name\":\"Stop\",\"session_id\":\"opaque/thread\"}}\n%4\n"
            )
        );
    }
}

#[test]
fn replacing_an_agent_generation_stops_its_managed_process_tree() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    let extension_root = directory.path().join("extensions");
    let parent_marker = directory.path().join("pi.pid");
    let child_marker = directory.path().join("pi-child.pid");
    let program = executable(
        directory.path(),
        "long-pi",
        &format!(
            "#!/bin/sh\nprintf '%s' $$ > '{}'\nsleep 300 &\nprintf '%s' $! > '{}'\nwait\n",
            parent_marker.display(),
            child_marker.display(),
        ),
    );
    let catalog = Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4, Arc::new(|| {}));
    let mut host = ExtensionHost::load(
        &extension_root,
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );
    assert_success(invoke(
        &catalog,
        "agents.pi.start",
        vec![path_text(directory.path()), path_text(&program)],
    ));
    let parent_pid = wait_for_pid(&catalog, &parent_marker);
    let child_pid = wait_for_pid(&catalog, &child_marker);

    fs::create_dir_all(extension_root.join("agents")).expect("agent extension directory");
    fs::write(
        extension_root.join("agents/pi.luau"),
        r#"
bootty.commands.register({ id = "agents.pi.state", title = "Replacement" }, function()
    return { status = "replacement" }
end)
"#,
    )
    .expect("replace Pi generation");
    host.refresh(Instant::now() + AGENT_BUDGET);

    let deadline = Instant::now() + AGENT_BUDGET;
    while process_exists(parent_pid) || process_exists(child_pid) {
        assert!(
            Instant::now() < deadline,
            "retired process tree stayed alive"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        success_value(invoke(&catalog, "agents.pi.state", Vec::new()))["status"],
        json!("replacement")
    );
}

#[test]
fn agent_stop_waits_for_the_process_tree_and_allows_an_immediate_restart() {
    let directory = tempfile::tempdir().expect("temporary extension root");
    let parent_marker = directory.path().join("pi-stop.pid");
    let child_marker = directory.path().join("pi-stop-child.pid");
    let program = executable(
        directory.path(),
        "stoppable-pi",
        &format!(
            "#!/bin/sh\nprintf '%s' $$ > '{}'\nsleep 300 &\nprintf '%s' $! > '{}'\nwait\n",
            parent_marker.display(),
            child_marker.display(),
        ),
    );
    let catalog = Arc::new(ExtensionCatalog::default());
    let (sender, _receiver) = app_command_channel(4, Arc::new(|| {}));
    let _host = ExtensionHost::load(
        &directory.path().join("extensions"),
        Arc::clone(&catalog),
        sender.for_caller(Caller::Luau),
        event_queue().0,
    );

    assert_success(invoke(
        &catalog,
        "agents.pi.start",
        vec![path_text(directory.path()), path_text(&program)],
    ));
    let first_parent = wait_for_pid(&catalog, &parent_marker);
    let first_child = wait_for_pid(&catalog, &child_marker);
    assert_success(invoke_confirmed(&catalog, "agents.pi.stop", Vec::new()));
    assert!(!process_exists(first_parent));
    assert!(!process_exists(first_child));

    fs::remove_file(&parent_marker).expect("remove first parent marker");
    fs::remove_file(&child_marker).expect("remove first child marker");
    assert_success(invoke(
        &catalog,
        "agents.pi.start",
        vec![path_text(directory.path()), path_text(&program)],
    ));
    let second_parent = wait_for_pid(&catalog, &parent_marker);
    let second_child = wait_for_pid(&catalog, &child_marker);
    assert!(process_exists(second_parent));
    assert!(process_exists(second_child));
    assert_success(invoke_confirmed(&catalog, "agents.pi.stop", Vec::new()));
    assert!(!process_exists(second_parent));
    assert!(!process_exists(second_child));
}

fn invoke(catalog: &ExtensionCatalog, command: &str, arguments: Vec<String>) -> CommandOutcome {
    invoke_with(catalog, command, arguments, false)
}

fn invoke_confirmed(
    catalog: &ExtensionCatalog,
    command: &str,
    arguments: Vec<String>,
) -> CommandOutcome {
    invoke_with(catalog, command, arguments, true)
}

fn invoke_with(
    catalog: &ExtensionCatalog,
    command: &str,
    arguments: Vec<String>,
    confirmed: bool,
) -> CommandOutcome {
    let mut invocation = CommandInvocation::new(command, arguments, Caller::Socket);
    if confirmed {
        invocation.confirmation = Some(invocation.confirmation());
    }
    let (_, sender) = catalog.command(command).expect("resolve agent command");
    sender
        .invoke(
            invocation,
            Instant::now() + AGENT_BUDGET,
            CommandCancellation::new(),
        )
        .recv_timeout(AGENT_BUDGET)
        .expect("agent command outcome")
}

fn wait_for_state(
    catalog: &ExtensionCatalog,
    command: &str,
    ready: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + AGENT_BUDGET;
    loop {
        let state = success_value(invoke(catalog, command, Vec::new()));
        if ready(&state) {
            return state;
        }
        assert_ne!(
            state["status"],
            json!("stopped"),
            "{command} reported a stopped agent process before the state became ready"
        );
        assert!(
            Instant::now() < deadline,
            "agent state did not become ready"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn success_value(outcome: CommandOutcome) -> Value {
    match outcome {
        CommandOutcome::Success { value, warnings } => {
            assert_eq!(warnings, Vec::new());
            value
        }
        other => panic!("expected success, got {other:?}"),
    }
}

fn assert_success(outcome: CommandOutcome) {
    let _ = success_value(outcome);
}

fn assert_failure_contains(outcome: CommandOutcome, expected: &str) {
    let CommandOutcome::Failed { message, .. } = outcome else {
        panic!("expected failure, got {outcome:?}");
    };
    assert!(
        message.contains(expected),
        "{message:?} does not contain {expected:?}"
    );
}

fn executable(directory: &Path, name: &str, source: &str) -> std::path::PathBuf {
    let path = directory.join(name);
    fs::write(&path, source).expect("write executable fixture");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("make fixture executable");
    path
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn wait_for_pid(catalog: &ExtensionCatalog, path: &Path) -> u32 {
    let deadline = Instant::now() + AGENT_BUDGET;
    loop {
        if let Ok(value) = fs::read_to_string(path)
            && let Ok(pid) = value.parse()
        {
            return pid;
        }
        assert_ne!(
            success_value(invoke(catalog, "agents.pi.state", Vec::new()))["status"],
            json!("stopped"),
            "fixture process exited before it recorded {}",
            path.display()
        );
        assert!(Instant::now() < deadline, "fixture process did not start");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn process_exists(pid: u32) -> bool {
    std::process::Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}
