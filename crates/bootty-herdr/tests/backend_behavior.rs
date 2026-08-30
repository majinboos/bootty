use std::{collections::BTreeMap, sync::Mutex};

use anyhow::Result;
use bootty_herdr::{
    HerdrApi, HerdrBackend, HerdrLayout, HerdrLayoutPane, HerdrLayoutSplit, HerdrPane, HerdrRect,
    HerdrSessionSnapshot, HerdrTab, HerdrWorkspace, RemoteHerdrBridgePlan, parse_remote_status,
    project_snapshot, remote_status_command,
};
use bootty_mux::{
    backend::MuxBackend,
    command::{MuxCommand, MuxSplitDirection},
    snapshot::{MuxPaneLayout, MuxPaneSplitDirection},
};
use bootty_mux_model::SshTarget;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

struct FakeApi {
    snapshot: HerdrSessionSnapshot,
    requests: Mutex<Vec<(String, Value)>>,
}

impl FakeApi {
    fn new(snapshot: HerdrSessionSnapshot) -> Self {
        Self {
            snapshot,
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl HerdrApi for FakeApi {
    fn snapshot(&self) -> Result<HerdrSessionSnapshot> {
        Ok(self.snapshot.clone())
    }
    fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.requests
            .lock()
            .expect("request lock")
            .push((method.into(), params));
        Ok(json!({"type": "ok"}))
    }
}

#[test]
fn projects_workspace_tab_pane_tags_and_bsp_layout() {
    let snapshot = fixture();
    assert_eq!(snapshot.terminal_id_for_pane("w1:p2"), Some("term-2"));
    assert_eq!(snapshot.terminal_id_for_pane("missing"), None);
    let projected = project_snapshot(&snapshot).expect("project snapshot");
    let session = &projected.sessions[0];
    assert_eq!(session.id, "w1");
    assert_eq!(session.name, "Bootty");
    assert_eq!(session.tag.identity.as_deref(), Some("identity"));
    assert_eq!(session.tag.space.as_deref(), Some("space"));
    let window = &session.windows[0];
    assert_eq!(window.id, "w1:t1");
    assert_eq!(
        window
            .panes
            .iter()
            .filter_map(|pane| pane.pane_id.as_deref())
            .collect::<Vec<_>>(),
        ["w1:p1", "w1:p2"]
    );
    assert_eq!(window.anchor.pane_id.as_deref(), Some("w1:p2"));
    assert_eq!(
        window.layout,
        Some(MuxPaneLayout::Split {
            direction: MuxPaneSplitDirection::Right,
            ratio_millis: 600,
            first: Box::new(MuxPaneLayout::Pane("w1:p1".into())),
            second: Box::new(MuxPaneLayout::Pane("w1:p2".into())),
        })
    );
}

#[test]
fn projects_zoomed_layout_as_the_focused_pane() {
    let mut snapshot = fixture();
    snapshot.layouts[0].zoomed = true;
    let projected = project_snapshot(&snapshot).expect("project snapshot");
    assert_eq!(
        projected.sessions[0].windows[0].layout,
        Some(MuxPaneLayout::Pane("w1:p2".into()))
    );
}

#[test]
fn commands_use_public_herdr_mutations() {
    let api = FakeApi::new(fixture());
    let mut backend = HerdrBackend::with_api(api);
    MuxBackend::execute(
        &mut backend,
        MuxCommand::SplitPane {
            session_id: "w1".into(),
            pane_id: Some("w1:p1".into()),
            direction: MuxSplitDirection::Down,
        },
    )
    .expect("split");
    MuxBackend::execute(
        &mut backend,
        MuxCommand::MoveWindow {
            session_id: "w1".into(),
            window_id: Some("w1:t1".into()),
            delta: 1,
        },
    )
    .expect("move");
    assert_eq!(
        *backend.api().requests.lock().expect("request lock"),
        [
            (
                "pane.split".into(),
                json!({"target_pane_id":"w1:p1", "direction":"down", "focus":true})
            ),
            (
                "tab.move".into(),
                json!({"tab_id":"w1:t1", "insert_index":0})
            ),
        ]
    );
}

#[test]
fn remote_bridge_forwards_both_public_herdr_sockets() {
    let target = SshTarget {
        host: "hermes".into(),
        user: Some("luan".into()),
        port: Some(2222),
        program: "ssh".into(),
        args: vec!["-F".into(), "/tmp/ssh-config".into()],
    };
    let plan = RemoteHerdrBridgePlan::new(
        &target,
        std::path::Path::new("/tmp/bootty-herdr-test"),
        std::path::Path::new("/home/luan/.config/herdr/sessions/work/herdr.sock"),
    )
    .expect("remote bridge plan");
    assert_eq!(
        plan.remote_client_socket,
        std::path::Path::new("/home/luan/.config/herdr/sessions/work/herdr-client.sock")
    );
    assert!(
        plan.arguments
            .windows(2)
            .any(|pair| pair == ["-o", "ExitOnForwardFailure=yes"])
    );
    assert_eq!(
        &plan.arguments[..4],
        ["-o", "ControlMaster=no", "-o", "ControlPath=none"]
    );
    assert!(
        plan.arguments
            .windows(2)
            .any(|pair| pair == ["-o", "StreamLocalBindUnlink=yes"])
    );
    assert!(
        plan.arguments.contains(
            &"/tmp/bootty-herdr-test/herdr.sock:/home/luan/.config/herdr/sessions/work/herdr.sock"
                .into()
        )
    );
    assert!(plan.arguments.contains(&"/tmp/bootty-herdr-test/herdr-client.sock:/home/luan/.config/herdr/sessions/work/herdr-client.sock".into()));
    assert_eq!(plan.arguments.iter().filter(|arg| *arg == "-N").count(), 1);
    assert_eq!(plan.arguments[plan.arguments.len() - 1], "luan@hermes");
}

#[test]
fn remote_status_requires_an_absolute_socket_for_a_running_server() {
    let status = parse_remote_status(
        r#"{"server":{"running":true,"socket":"/run/user/501/herdr/herdr.sock"}}"#,
    )
    .expect("remote status");
    assert!(status.running);
    assert_eq!(
        status.socket.as_deref(),
        Some(std::path::Path::new("/run/user/501/herdr/herdr.sock"))
    );
    assert!(
        parse_remote_status(r#"{"server":{"running":true,"socket":"relative.sock"}}"#).is_err()
    );
    assert!(
        !parse_remote_status(r#"{"server":{"running":false,"socket":null}}"#)
            .expect("stopped status")
            .running
    );
}

#[test]
fn remote_status_command_targets_the_selected_session_without_a_daemon() {
    let target = SshTarget {
        host: "hermes".into(),
        user: None,
        port: Some(2202),
        program: "/opt/ssh".into(),
        args: vec!["-F".into(), "/tmp/ssh-config".into()],
    };
    let (program, arguments) =
        remote_status_command(&target, "bootty-work").expect("remote status command");
    assert_eq!(program, "/opt/ssh");
    assert!(arguments.windows(2).any(|pair| pair == ["-p", "2202"]));
    assert!(
        arguments
            .windows(2)
            .any(|pair| pair == ["-F", "/tmp/ssh-config"])
    );
    assert_eq!(arguments[arguments.len() - 2], "hermes");
    assert_eq!(
        arguments.last().map(String::as_str),
        Some("'herdr' '--session' 'bootty-work' 'status' '--json'")
    );
    assert!(
        !arguments
            .iter()
            .any(|argument| argument.contains("bootty-remote"))
    );
}

#[test]
fn remote_bootstrap_starts_only_the_headless_named_server() {
    let target = SshTarget {
        host: "hermes".into(),
        user: Some("luan".into()),
        port: Some(2222),
        program: "/opt/ssh".into(),
        args: vec!["-F".into(), "/tmp/ssh-config".into()],
    };
    let (program, arguments) =
        RemoteHerdrBridgePlan::server_bootstrap_command(&target, "bootty-work")
            .expect("remote bootstrap command");
    assert_eq!(program, "/opt/ssh");
    assert!(arguments.windows(2).any(|pair| pair == ["-p", "2222"]));
    assert!(
        arguments
            .windows(2)
            .any(|pair| pair == ["-F", "/tmp/ssh-config"])
    );
    assert_eq!(arguments[arguments.len() - 2], "luan@hermes");
    let command = arguments.last().expect("remote command");
    assert!(command.contains("'nohup herdr --session \"$1\" server"));
    assert!(command.contains("api snapshot"));
    assert!(command.ends_with("'bootty-work'"));
    assert!(!command.contains("attach"));
}

fn fixture() -> HerdrSessionSnapshot {
    HerdrSessionSnapshot {
        version: "0.8.2".into(),
        protocol: 20,
        focused_workspace_id: Some("w1".into()),
        focused_tab_id: Some("w1:t1".into()),
        focused_pane_id: Some("w1:p2".into()),
        workspaces: vec![HerdrWorkspace {
            workspace_id: "w1".into(),
            number: 0,
            label: "Bootty".into(),
            focused: true,
            active_tab_id: "w1:t1".into(),
            agent_status: "working".into(),
            tokens: BTreeMap::from([
                ("bootty_id".into(), "identity".into()),
                ("bootty_space".into(), "space".into()),
            ]),
        }],
        tabs: vec![HerdrTab {
            tab_id: "w1:t1".into(),
            workspace_id: "w1".into(),
            number: 0,
            label: "code".into(),
            focused: true,
            agent_status: "working".into(),
        }],
        panes: vec![
            HerdrPane {
                pane_id: "w1:p1".into(),
                terminal_id: "term-1".into(),
                workspace_id: "w1".into(),
                tab_id: "w1:t1".into(),
                focused: false,
                cwd: Some("/repo".into()),
                foreground_cwd: None,
                agent: None,
                display_agent: None,
                title: None,
                agent_status: "idle".into(),
                revision: 1,
            },
            HerdrPane {
                pane_id: "w1:p2".into(),
                terminal_id: "term-2".into(),
                workspace_id: "w1".into(),
                tab_id: "w1:t1".into(),
                focused: true,
                cwd: Some("/repo".into()),
                foreground_cwd: None,
                agent: Some("codex".into()),
                display_agent: Some("Codex".into()),
                title: None,
                agent_status: "working".into(),
                revision: 2,
            },
        ],
        layouts: vec![HerdrLayout {
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
            zoomed: false,
            focused_pane_id: "w1:p2".into(),
            panes: vec![
                HerdrLayoutPane {
                    pane_id: "w1:p1".into(),
                    focused: false,
                    rect: HerdrRect {
                        x: 0,
                        y: 0,
                        width: 60,
                        height: 24,
                    },
                },
                HerdrLayoutPane {
                    pane_id: "w1:p2".into(),
                    focused: true,
                    rect: HerdrRect {
                        x: 60,
                        y: 0,
                        width: 40,
                        height: 24,
                    },
                },
            ],
            splits: vec![HerdrLayoutSplit {
                id: "split_0_root".into(),
                direction: "right".into(),
                ratio: 0.6,
                rect: HerdrRect {
                    x: 0,
                    y: 0,
                    width: 100,
                    height: 24,
                },
            }],
        }],
        agents: Vec::new(),
    }
}
