use anyhow::{Context, Result};

use super::{
    backend::MuxBackend,
    command::MuxCommand,
    config::MuxBackendKind,
    process::{CommandRunner, SystemCommandRunner, require_success},
    snapshot::{MuxPaneAnchor, MuxSession, MuxSnapshot, MuxWindow},
};

#[derive(Clone, Debug)]
pub struct TmuxBackend<R = SystemCommandRunner> {
    program: String,
    runner: R,
}

impl TmuxBackend<SystemCommandRunner> {
    pub fn new() -> Self {
        Self::with_runner("tmux", SystemCommandRunner)
    }
}

impl Default for TmuxBackend<SystemCommandRunner> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R> TmuxBackend<R> {
    pub fn with_runner(program: impl Into<String>, runner: R) -> Self {
        Self {
            program: program.into(),
            runner,
        }
    }
}

impl<R: CommandRunner> TmuxBackend<R> {
    fn run(&self, args: &[&str]) -> Result<String> {
        let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        let output = self.runner.run(&self.program, &args)?;
        require_success(&self.program, &args, output)
    }

    fn run_owned(&self, args: Vec<String>) -> Result<String> {
        let output = self.runner.run(&self.program, &args)?;
        require_success(&self.program, &args, output)
    }
}

impl<R: CommandRunner> MuxBackend for TmuxBackend<R> {
    fn kind(&self) -> MuxBackendKind {
        MuxBackendKind::Tmux
    }

    fn snapshot(&self) -> Result<MuxSnapshot> {
        let sessions = self.run(&[
            "list-sessions",
            "-F",
            "#{session_id}\t#{session_name}\t#{session_attached}\t#{session_windows}\t#{pane_id}\t#{pane_current_path}\t#{pane_current_command}",
        ])?;
        let panes = self.run(&[
            "list-panes",
            "-a",
            "-F",
            "#{session_id}\t#{window_id}\t#{window_index}\t#{window_name}\t#{window_active}\t#{pane_active}\t#{pane_id}\t#{pane_current_path}\t#{pane_current_command}",
        ])?;
        parse_tmux_snapshot(&sessions, &panes)
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        match command {
            MuxCommand::ActivateSession { session_id } => {
                self.run_owned(vec!["switch-client".into(), "-t".into(), session_id])?;
            }
            MuxCommand::ActivateWindow {
                session_id: _,
                window_id,
            } => {
                self.run_owned(vec!["select-window".into(), "-t".into(), window_id])?;
            }
            MuxCommand::CreateProjectSession { session_id, cwd }
            | MuxCommand::CreateWorktreeSession { session_id, cwd } => {
                self.run_owned(vec![
                    "new-session".into(),
                    "-d".into(),
                    "-s".into(),
                    session_id,
                    "-c".into(),
                    cwd,
                ])?;
            }
            MuxCommand::RenameSession { session_id, name } => {
                self.run_owned(vec!["rename-session".into(), "-t".into(), session_id, name])?;
            }
            MuxCommand::DitchSession { session_id } => {
                self.run_owned(vec!["kill-session".into(), "-t".into(), session_id])?;
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
                anyhow::bail!("tmux backend does not support mux command {command:?}");
            }
        }
        Ok(())
    }
}

fn parse_tmux_snapshot(sessions_output: &str, panes_output: &str) -> Result<MuxSnapshot> {
    let mut sessions = Vec::new();
    for line in sessions_output
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        let mut fields = line.split('\t');
        let id = fields.next().context("tmux snapshot missing session id")?;
        let name = fields
            .next()
            .context("tmux snapshot missing session name")?;
        let attached = fields.next().unwrap_or("0") != "0";
        let _windows = fields.next();
        let pane_id = fields.next().filter(|value| !value.is_empty());
        let cwd = fields.next().filter(|value| !value.is_empty());
        let process = fields.next().filter(|value| !value.is_empty());
        sessions.push(MuxSession {
            id: id.to_owned(),
            name: name.to_owned(),
            active: attached,
            anchor: MuxPaneAnchor {
                session_id: id.to_owned(),
                pane_id: pane_id.map(str::to_owned),
                cwd: cwd.map(str::to_owned),
                process: process.map(str::to_owned),
            },
            active_window_id: None,
            windows: Vec::new(),
        });
    }
    add_tmux_windows(&mut sessions, panes_output)?;

    Ok(MuxSnapshot {
        active_session_id: sessions
            .iter()
            .find(|session| session.active)
            .map(|session| session.id.clone()),
        sessions,
    })
}

fn add_tmux_windows(sessions: &mut [MuxSession], panes_output: &str) -> Result<()> {
    for line in panes_output.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.split('\t');
        let session_id = fields.next().context("tmux pane missing session id")?;
        let window_id = fields.next().context("tmux pane missing window id")?;
        let window_index = fields
            .next()
            .context("tmux pane missing window index")?
            .parse()
            .context("tmux pane window index is not a number")?;
        let window_name = fields.next().context("tmux pane missing window name")?;
        let window_active = fields.next().unwrap_or("0") != "0";
        let pane_active = fields.next().unwrap_or("0") != "0";
        let pane_id = fields.next().filter(|value| !value.is_empty());
        let cwd = fields.next().filter(|value| !value.is_empty());
        let process = fields.next().filter(|value| !value.is_empty());

        let Some(session) = sessions.iter_mut().find(|session| session.id == session_id) else {
            continue;
        };
        if window_active {
            session.active_window_id = Some(window_id.to_owned());
        }
        if let Some(window) = session
            .windows
            .iter_mut()
            .find(|window| window.id == window_id)
        {
            if pane_active || window.anchor.pane_id.is_none() {
                window.anchor = MuxPaneAnchor {
                    session_id: session_id.to_owned(),
                    pane_id: pane_id.map(str::to_owned),
                    cwd: cwd.map(str::to_owned),
                    process: process.map(str::to_owned),
                };
            }
            continue;
        }
        session.windows.push(MuxWindow {
            id: window_id.to_owned(),
            index: window_index,
            name: window_name.to_owned(),
            active: window_active,
            anchor: MuxPaneAnchor {
                session_id: session_id.to_owned(),
                pane_id: pane_id.map(str::to_owned),
                cwd: cwd.map(str::to_owned),
                process: process.map(str::to_owned),
            },
        });
    }
    for session in sessions {
        session.windows.sort_by_key(|window| window.index);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::VecDeque, rc::Rc};

    use super::*;
    use crate::mux::process::{CommandOutput, CommandRunner};

    #[derive(Clone, Default)]
    struct RecordingRunner {
        calls: Rc<RefCell<Vec<Vec<String>>>>,
        stdout: Rc<RefCell<VecDeque<String>>>,
    }

    impl CommandRunner for RecordingRunner {
        fn run(&self, program: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
            let mut call = vec![program.to_owned()];
            call.extend(args.iter().cloned());
            self.calls.borrow_mut().push(call);
            Ok(CommandOutput {
                success: true,
                stdout: self.stdout.borrow_mut().pop_front().unwrap_or_default(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn tmux_adapter_translates_lifecycle_commands() {
        let runner = RecordingRunner::default();
        let calls = runner.calls.clone();
        let mut backend = TmuxBackend::with_runner("tmux", runner);

        backend
            .execute(MuxCommand::ActivateSession {
                session_id: "proj".to_owned(),
            })
            .unwrap();
        backend
            .execute(MuxCommand::ActivateWindow {
                session_id: "$1".to_owned(),
                window_id: "@2".to_owned(),
            })
            .unwrap();
        backend
            .execute(MuxCommand::CreateProjectSession {
                session_id: "proj".to_owned(),
                cwd: "/repo".to_owned(),
            })
            .unwrap();
        backend
            .execute(MuxCommand::RenameSession {
                session_id: "proj".to_owned(),
                name: "next".to_owned(),
            })
            .unwrap();
        backend
            .execute(MuxCommand::DitchSession {
                session_id: "next".to_owned(),
            })
            .unwrap();

        assert_eq!(
            calls.borrow().as_slice(),
            vec![
                vec!["tmux", "switch-client", "-t", "proj"],
                vec!["tmux", "select-window", "-t", "@2"],
                vec!["tmux", "new-session", "-d", "-s", "proj", "-c", "/repo"],
                vec!["tmux", "rename-session", "-t", "proj", "next"],
                vec!["tmux", "kill-session", "-t", "next"],
            ]
            .into_iter()
            .map(|call| call.into_iter().map(str::to_owned).collect::<Vec<_>>())
            .collect::<Vec<_>>()
            .as_slice()
        );
    }

    #[test]
    fn tmux_snapshot_maps_sessions_and_metadata_anchors() {
        let runner = RecordingRunner {
            calls: Rc::default(),
            stdout: Rc::new(RefCell::new(VecDeque::from([
                "$1\talpha\t1\t2\t%3\t/repo\tzsh\n$2\tbeta\t0\t1\t%4\t/tmp\tfish\n"
                    .to_owned(),
                "$1\t@1\t0\teditor\t1\t1\t%3\t/repo\tnvim\n$1\t@2\t1\tshell\t0\t1\t%5\t/repo\tzsh\n$2\t@3\t0\tlogs\t1\t1\t%4\t/tmp\tfish\n"
                    .to_owned(),
            ]))),
        };
        let backend = TmuxBackend::with_runner("tmux", runner);

        let snapshot = backend.snapshot().unwrap();

        assert_eq!(snapshot.active_session_id.as_deref(), Some("$1"));
        assert_eq!(snapshot.sessions[0].name, "alpha");
        assert_eq!(snapshot.sessions[0].anchor.pane_id.as_deref(), Some("%3"));
        assert_eq!(snapshot.sessions[0].anchor.cwd.as_deref(), Some("/repo"));
        assert_eq!(snapshot.sessions[0].anchor.process.as_deref(), Some("zsh"));
        assert_eq!(snapshot.sessions[0].active_window_id.as_deref(), Some("@1"));
        assert_eq!(snapshot.sessions[0].windows.len(), 2);
        assert_eq!(snapshot.sessions[0].windows[0].name, "editor");
        assert_eq!(
            snapshot.sessions[0].windows[0].anchor.pane_id.as_deref(),
            Some("%3")
        );
        assert_eq!(snapshot.sessions[0].windows[1].name, "shell");
    }
}
