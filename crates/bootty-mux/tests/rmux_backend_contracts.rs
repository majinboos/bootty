use std::{cell::RefCell, rc::Rc};

use anyhow::Result;
use bootty_mux::{
    command::{MuxCommand, MuxSplitDirection},
    rmux::{RmuxBackend, RmuxSessionClient},
    snapshot::MuxSnapshot,
};

#[derive(Clone, Default)]
struct RecordingClient {
    calls: Rc<RefCell<Vec<Vec<String>>>>,
    snapshot: MuxSnapshot,
}

impl RecordingClient {
    fn record(&self, operation: &str, fields: impl IntoIterator<Item = String>) {
        self.calls.borrow_mut().push(
            std::iter::once(operation.to_owned())
                .chain(fields)
                .collect(),
        );
    }
}

impl RmuxSessionClient for RecordingClient {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        self.record("snapshot", []);
        Ok(self.snapshot.clone())
    }

    fn ensure_session(&self, session: &str, cwd: &str) -> Result<()> {
        self.record("ensure_session", [session.to_owned(), cwd.to_owned()]);
        Ok(())
    }

    fn rename_session(&self, session: &str, name: &str) -> Result<()> {
        self.record("rename_session", [session.to_owned(), name.to_owned()]);
        Ok(())
    }

    fn kill_session(&self, session: &str) -> Result<()> {
        self.record("kill_session", [session.to_owned()]);
        Ok(())
    }

    fn activate_window(&self, session: &str, window: &str) -> Result<()> {
        self.record("activate_window", [session.to_owned(), window.to_owned()]);
        Ok(())
    }

    fn rename_window(&self, session: &str, window: &str, name: &str) -> Result<()> {
        self.record(
            "rename_window",
            [session.to_owned(), window.to_owned(), name.to_owned()],
        );
        Ok(())
    }

    fn new_window(&self, session: &str, cwd: Option<&str>) -> Result<()> {
        self.record(
            "new_window",
            [session.to_owned(), cwd.unwrap_or_default().to_owned()],
        );
        Ok(())
    }

    fn activate_next_window(&self, session: &str) -> Result<()> {
        self.record("activate_next_window", [session.to_owned()]);
        Ok(())
    }

    fn activate_previous_window(&self, session: &str) -> Result<()> {
        self.record("activate_previous_window", [session.to_owned()]);
        Ok(())
    }

    fn activate_last_window(&self, session: &str) -> Result<()> {
        self.record("activate_last_window", [session.to_owned()]);
        Ok(())
    }

    fn activate_window_index(&self, session: &str, index: u32) -> Result<()> {
        self.record(
            "activate_window_index",
            [session.to_owned(), index.to_string()],
        );
        Ok(())
    }

    fn move_window(&self, session: &str, window: Option<&str>, delta: i32) -> Result<()> {
        self.record(
            "move_window",
            [
                session.to_owned(),
                window.unwrap_or_default().to_owned(),
                delta.to_string(),
            ],
        );
        Ok(())
    }

    fn split_pane(
        &self,
        session: &str,
        pane: Option<&str>,
        direction: MuxSplitDirection,
    ) -> Result<()> {
        self.record(
            "split_pane",
            [
                session.to_owned(),
                pane.unwrap_or_default().to_owned(),
                format!("{direction:?}"),
            ],
        );
        Ok(())
    }

    fn close_pane(&self, session: &str, pane: Option<&str>) -> Result<()> {
        self.record(
            "close_pane",
            [session.to_owned(), pane.unwrap_or_default().to_owned()],
        );
        Ok(())
    }
}

#[test]
fn rmux_backend_maps_session_window_and_pane_commands_to_the_client() {
    let client = RecordingClient::default();
    let calls = client.calls.clone();
    let mut backend = RmuxBackend::with_client(client);

    for command in [
        MuxCommand::CreateProjectSession {
            session_id: "project".to_owned(),
            cwd: "/repo".to_owned(),
        },
        MuxCommand::RenameSession {
            session_id: "project".to_owned(),
            name: "renamed".to_owned(),
        },
        MuxCommand::NewWindow {
            session_id: "project".to_owned(),
            cwd: Some("/repo/work".to_owned()),
        },
        MuxCommand::SplitPane {
            session_id: "project".to_owned(),
            pane_id: Some("%3".to_owned()),
            direction: MuxSplitDirection::Down,
        },
        MuxCommand::MoveWindowPreservingSelection {
            session_id: "project".to_owned(),
            window_id: "@2".to_owned(),
            delta: -1,
            selected_window_id: "@4".to_owned(),
        },
        MuxCommand::ClosePane {
            session_id: "project".to_owned(),
            pane_id: Some("%5".to_owned()),
        },
        MuxCommand::DitchSession {
            session_id: "project".to_owned(),
        },
    ] {
        backend.execute(command).unwrap();
    }

    assert_eq!(
        calls.borrow().as_slice(),
        [
            ["ensure_session", "project", "/repo"].as_slice(),
            ["rename_session", "project", "renamed"].as_slice(),
            ["new_window", "project", "/repo/work"].as_slice(),
            ["split_pane", "project", "%3", "Down"].as_slice(),
            ["move_window", "project", "@2", "-1"].as_slice(),
            ["activate_window", "project", "@4"].as_slice(),
            ["close_pane", "project", "%5"].as_slice(),
            ["kill_session", "project"].as_slice(),
        ]
        .map(|fields| fields
            .iter()
            .map(|field| (*field).to_owned())
            .collect::<Vec<_>>())
        .as_slice()
    );
}

#[test]
fn rmux_backend_returns_the_client_snapshot_without_a_second_model() {
    let expected = MuxSnapshot {
        active_session_id: Some("project".to_owned()),
        ..MuxSnapshot::default()
    };
    let backend = RmuxBackend::with_client(RecordingClient {
        calls: Rc::default(),
        snapshot: expected.clone(),
    });

    assert_eq!(backend.snapshot().unwrap(), expected);
}
