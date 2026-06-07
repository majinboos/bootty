use anyhow::Result;

use super::{command::MuxCommand, config::MuxBackendKind, snapshot::MuxSnapshot};

pub trait MuxBackend {
    fn kind(&self) -> MuxBackendKind;
    fn snapshot(&self) -> Result<MuxSnapshot>;
    fn execute(&mut self, command: MuxCommand) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::snapshot::{MuxPaneAnchor, MuxSession};

    #[derive(Default)]
    struct FakeBackend {
        sessions: Vec<MuxSession>,
        commands: Vec<MuxCommand>,
    }

    impl MuxBackend for FakeBackend {
        fn kind(&self) -> MuxBackendKind {
            MuxBackendKind::Rmux
        }

        fn snapshot(&self) -> Result<MuxSnapshot> {
            Ok(MuxSnapshot {
                active_session_id: self
                    .sessions
                    .iter()
                    .find(|session| session.active)
                    .map(|session| session.id.clone()),
                sessions: self.sessions.clone(),
            })
        }

        fn execute(&mut self, command: MuxCommand) -> Result<()> {
            self.commands.push(command);
            Ok(())
        }
    }

    #[test]
    fn fake_backend_contract_covers_session_lifecycle_and_anchors() {
        let mut backend = FakeBackend {
            sessions: vec![MuxSession {
                id: "project".to_owned(),
                name: "project".to_owned(),
                active: true,
                anchor: MuxPaneAnchor {
                    session_id: "project".to_owned(),
                    pane_id: Some("pane-1".to_owned()),
                    cwd: Some("/repo".to_owned()),
                    process: Some("zsh".to_owned()),
                },
                active_window_id: None,
                windows: Vec::new(),
            }],
            commands: Vec::new(),
        };

        let snapshot = backend.snapshot().unwrap();
        assert_eq!(snapshot.active_session_id.as_deref(), Some("project"));
        assert_eq!(snapshot.sessions[0].anchor.cwd.as_deref(), Some("/repo"));

        let commands = [
            MuxCommand::ActivateSession {
                session_id: "project".to_owned(),
            },
            MuxCommand::ActivateWindow {
                session_id: "project".to_owned(),
                window_id: "@1".to_owned(),
            },
            MuxCommand::CreateProjectSession {
                session_id: "next".to_owned(),
                cwd: "/next".to_owned(),
            },
            MuxCommand::CreateWorktreeSession {
                session_id: "worktree".to_owned(),
                cwd: "/repo-worktree".to_owned(),
            },
            MuxCommand::RenameSession {
                session_id: "project".to_owned(),
                name: "renamed".to_owned(),
            },
            MuxCommand::DitchSession {
                session_id: "renamed".to_owned(),
            },
        ];
        for command in commands.clone() {
            backend.execute(command).unwrap();
        }

        assert_eq!(backend.commands, commands);
    }
}
