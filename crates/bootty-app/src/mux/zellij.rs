use anyhow::Result;

use super::{
    backend::MuxBackend,
    command::MuxCommand,
    config::MuxBackendKind,
    process::{CommandRunner, SystemCommandRunner, require_success},
    snapshot::{MuxPaneAnchor, MuxSession, MuxSnapshot},
};

#[derive(Clone, Debug)]
pub struct ZellijBackend<R = SystemCommandRunner> {
    runner: R,
}

impl ZellijBackend<SystemCommandRunner> {
    pub fn new() -> Self {
        Self::with_runner(SystemCommandRunner)
    }
}

impl Default for ZellijBackend<SystemCommandRunner> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R> ZellijBackend<R> {
    pub fn with_runner(runner: R) -> Self {
        Self { runner }
    }
}

impl<R: CommandRunner> ZellijBackend<R> {
    fn run(&self, args: &[&str]) -> Result<String> {
        let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        let output = self.runner.run("zellij", &args)?;
        require_success("zellij", &args, output)
    }

    fn run_owned(&self, args: Vec<String>) -> Result<String> {
        let output = self.runner.run("zellij", &args)?;
        require_success("zellij", &args, output)
    }
}

impl<R: CommandRunner> MuxBackend for ZellijBackend<R> {
    fn kind(&self) -> MuxBackendKind {
        MuxBackendKind::Zellij
    }

    fn snapshot(&self) -> Result<MuxSnapshot> {
        let output = self.run(&["list-sessions", "--short", "--no-formatting"])?;
        Ok(parse_zellij_snapshot(&output))
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        match command {
            MuxCommand::ActivateSession { session_id } => {
                self.run_owned(vec!["action".into(), "switch-session".into(), session_id])?;
            }
            MuxCommand::ActivateWindow { .. } => {
                anyhow::bail!("zellij native window activation is not implemented");
            }
            MuxCommand::CreateProjectSession { session_id, cwd }
            | MuxCommand::CreateWorktreeSession { session_id, cwd } => {
                self.run_owned(vec![
                    "--layout-string".into(),
                    "layout {\n  pane\n}".into(),
                    "attach".into(),
                    "--create-background".into(),
                    session_id,
                    "options".into(),
                    "--pane-frames".into(),
                    "false".into(),
                    "--simplified-ui".into(),
                    "true".into(),
                    "--show-startup-tips".into(),
                    "false".into(),
                    "--default-cwd".into(),
                    cwd,
                ])?;
            }
            MuxCommand::RenameSession { session_id, name } => {
                self.run_owned(vec!["action".into(), "switch-session".into(), session_id])?;
                self.run_owned(vec!["action".into(), "rename-session".into(), name])?;
            }
            MuxCommand::DitchSession { session_id } => {
                self.run_owned(vec!["kill-session".into(), session_id])?;
            }
            MuxCommand::ActivateNextSession
            | MuxCommand::ActivatePreviousSession
            | MuxCommand::ActivateLastSession
            | MuxCommand::ActivateSessionIndex { .. }
            | MuxCommand::MoveSession { .. }
            | MuxCommand::NewWindow { .. }
            | MuxCommand::ActivateNextWindow { .. }
            | MuxCommand::ActivatePreviousWindow { .. }
            | MuxCommand::ActivateLastWindow { .. }
            | MuxCommand::ActivateWindowIndex { .. }
            | MuxCommand::MoveWindow { .. }
            | MuxCommand::SplitPane { .. }
            | MuxCommand::SelectPane { .. }
            | MuxCommand::SelectNextPane { .. }
            | MuxCommand::KillPane { .. }
            | MuxCommand::TogglePaneZoom { .. } => {
                anyhow::bail!("zellij backend does not support mux command {command:?}");
            }
        }
        Ok(())
    }
}

fn parse_zellij_snapshot(output: &str) -> MuxSnapshot {
    let sessions = output
        .lines()
        .filter_map(|line| {
            let name = line.trim();
            if name.is_empty() || name.starts_with("No active zellij sessions") {
                return None;
            }
            Some(MuxSession {
                id: name.to_owned(),
                name: name.to_owned(),
                active: false,
                anchor: MuxPaneAnchor {
                    session_id: name.to_owned(),
                    pane_id: None,
                    cwd: None,
                    process: None,
                },
                active_window_id: None,
                windows: Vec::new(),
            })
        })
        .collect();
    MuxSnapshot {
        sessions,
        active_session_id: None,
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use super::*;
    use crate::mux::{
        command::MuxCommand,
        process::{CommandOutput, CommandRunner},
    };

    #[derive(Clone, Default)]
    struct RecordingRunner {
        calls: Rc<RefCell<Vec<Vec<String>>>>,
        stdout: String,
    }

    impl CommandRunner for RecordingRunner {
        fn run(&self, program: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
            let mut call = vec![program.to_owned()];
            call.extend(args.iter().cloned());
            self.calls.borrow_mut().push(call);
            Ok(CommandOutput {
                success: true,
                stdout: self.stdout.clone(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn zellij_adapter_translates_lifecycle_without_tmux_fallback() {
        let runner = RecordingRunner::default();
        let calls = runner.calls.clone();
        let mut backend = ZellijBackend::with_runner(runner);

        backend
            .execute(MuxCommand::ActivateSession {
                session_id: "project".to_owned(),
            })
            .unwrap();
        backend
            .execute(MuxCommand::CreateProjectSession {
                session_id: "next".to_owned(),
                cwd: "/next".to_owned(),
            })
            .unwrap();
        backend
            .execute(MuxCommand::DitchSession {
                session_id: "next".to_owned(),
            })
            .unwrap();
        backend
            .execute(MuxCommand::RenameSession {
                session_id: "project".to_owned(),
                name: "renamed".to_owned(),
            })
            .unwrap();

        assert_eq!(
            calls.borrow().as_slice(),
            vec![
                vec!["zellij", "action", "switch-session", "project"],
                vec![
                    "zellij",
                    "--layout-string",
                    "layout {\n  pane\n}",
                    "attach",
                    "--create-background",
                    "next",
                    "options",
                    "--pane-frames",
                    "false",
                    "--simplified-ui",
                    "true",
                    "--show-startup-tips",
                    "false",
                    "--default-cwd",
                    "/next"
                ],
                vec!["zellij", "kill-session", "next"],
                vec!["zellij", "action", "switch-session", "project"],
                vec!["zellij", "action", "rename-session", "renamed"],
            ]
            .into_iter()
            .map(|call| call.into_iter().map(str::to_owned).collect::<Vec<_>>())
            .collect::<Vec<_>>()
            .as_slice()
        );
    }

    #[test]
    fn zellij_snapshot_maps_list_sessions_without_active_fallback() {
        let runner = RecordingRunner {
            calls: Rc::default(),
            stdout: "alpha\nbeta\n".to_owned(),
        };
        let backend = ZellijBackend::with_runner(runner);

        let snapshot = backend.snapshot().unwrap();

        assert_eq!(snapshot.active_session_id, None);
        assert_eq!(snapshot.sessions[0].id, "alpha");
        assert_eq!(snapshot.sessions[0].anchor.session_id, "alpha");
    }
}
