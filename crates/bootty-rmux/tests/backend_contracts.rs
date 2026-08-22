use std::{cell::RefCell, rc::Rc};

use anyhow::Result;
use bootty_mux::{
    backend::MuxBackend,
    command::{MuxCommand, MuxDirection, MuxSplitDirection},
    snapshot::{
        MuxPaneAnchor, MuxSession, MuxSessionTag, MuxSnapshot, MuxSnapshotDisposition, MuxWindow,
    },
};
use bootty_rmux::RmuxBackend;

#[derive(Clone, Default)]
struct RecordingControl {
    calls: Rc<RefCell<Vec<MuxCommand>>>,
    snapshot: MuxSnapshot,
}

impl MuxBackend for RecordingControl {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        Ok(self.snapshot.clone())
    }
    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        self.calls.borrow_mut().push(command);
        Ok(())
    }
}

#[test]
fn rmux_backend_forwards_every_control_command_unchanged() {
    let control = RecordingControl::default();
    let calls = control.calls.clone();
    let mut backend = RmuxBackend::with_control(control);

    let commands = vec![
        MuxCommand::ActivateWindow {
            session_id: "project".into(),
            window_id: "@1".into(),
        },
        MuxCommand::NewWindow {
            session_id: "project".into(),
            cwd: None,
        },
        MuxCommand::RenameWindow {
            session_id: "project".into(),
            window_id: "@1".into(),
            name: "tab".into(),
        },
        MuxCommand::ActivateNextWindow {
            session_id: "project".into(),
        },
        MuxCommand::ActivatePreviousWindow {
            session_id: "project".into(),
        },
        MuxCommand::ActivateLastWindow {
            session_id: "project".into(),
        },
        MuxCommand::ActivateWindowIndex {
            session_id: "project".into(),
            index: 2,
        },
        MuxCommand::MoveWindow {
            session_id: "project".into(),
            window_id: Some("@2".into()),
            delta: 1,
        },
        MuxCommand::MoveWindowPreservingSelection {
            session_id: "project".into(),
            window_id: "@2".into(),
            delta: -1,
            selected_window_id: "@4".into(),
        },
        MuxCommand::SplitPane {
            session_id: "project".into(),
            pane_id: Some("%3".into()),
            direction: MuxSplitDirection::Down,
        },
        MuxCommand::SelectPane {
            session_id: "project".into(),
            window_id: None,
            direction: MuxDirection::Right,
        },
        MuxCommand::SelectNextPane {
            session_id: "project".into(),
            window_id: Some("@1".into()),
        },
        MuxCommand::SelectPreviousPane {
            session_id: "project".into(),
            window_id: None,
        },
        MuxCommand::KillPane {
            session_id: "project".into(),
            pane_id: Some("%5".into()),
        },
        MuxCommand::ClosePane {
            session_id: "project".into(),
            pane_id: Some("%5".into()),
        },
        MuxCommand::TogglePaneZoom {
            session_id: "project".into(),
            pane_id: None,
        },
        MuxCommand::CreateProjectSession {
            session_id: "project".to_owned(),
            cwd: "/repo".to_owned(),
            tag: MuxSessionTag::default(),
        },
        MuxCommand::CreateWorktreeSession {
            session_id: "worktree".to_owned(),
            cwd: "/repo/worktree".to_owned(),
            tag: MuxSessionTag::default(),
        },
        MuxCommand::RenameSession {
            session_id: "project".to_owned(),
            name: "renamed".to_owned(),
        },
        MuxCommand::DitchSession {
            session_id: "project".to_owned(),
        },
    ];
    for command in commands.iter().cloned() {
        backend.execute(command).unwrap();
    }

    assert_eq!(calls.borrow().as_slice(), commands.as_slice());
}

#[test]
fn rmux_backend_returns_the_control_snapshot_without_a_second_model() {
    let expected = MuxSnapshot {
        active_session_id: Some("project".to_owned()),
        ..MuxSnapshot::default()
    };
    let backend = RmuxBackend::with_control(RecordingControl {
        calls: Rc::default(),
        snapshot: expected.clone(),
    });

    assert_eq!(backend.snapshot().unwrap(), expected);
}

fn session_with_panes(panes: Vec<MuxPaneAnchor>) -> MuxSession {
    MuxSession {
        id: "session".to_owned(),
        name: "session".to_owned(),
        active: true,
        anchor: MuxPaneAnchor::default(),
        active_window_id: Some("window".to_owned()),
        windows: vec![MuxWindow {
            id: "window".to_owned(),
            index: 0,
            name: "window".to_owned(),
            active: true,
            anchor: MuxPaneAnchor::default(),
            panes,
            layout: None,
            progress: None,
        }],
        tag: MuxSessionTag::default(),
    }
}

#[test]
fn rmux_backend_classifies_snapshot_disposition_from_control_sessions() {
    for (sessions, disposition) in [
        (Vec::new(), MuxSnapshotDisposition::Authoritative),
        (
            vec![session_with_panes(vec![MuxPaneAnchor::default()])],
            MuxSnapshotDisposition::Authoritative,
        ),
        (
            vec![session_with_panes(Vec::new())],
            MuxSnapshotDisposition::Transient,
        ),
        (
            vec![
                session_with_panes(vec![MuxPaneAnchor::default()]),
                session_with_panes(Vec::new()),
            ],
            MuxSnapshotDisposition::Transient,
        ),
    ] {
        let backend = RmuxBackend::with_control(RecordingControl {
            calls: Rc::default(),
            snapshot: MuxSnapshot {
                sessions,
                ..MuxSnapshot::default()
            },
        });
        assert_eq!(backend.snapshot().unwrap().disposition, disposition);
    }
}
