use anyhow::{Context, Result};

use super::{
    backend::MuxBackend,
    command::{MuxCommand, MuxDirection, MuxSplitDirection},
    process::{CommandRunner, SystemCommandRunner, require_success},
    snapshot::{MuxPaneAnchor, MuxSession, MuxSnapshot, MuxWindow, MuxWindowProgress},
};

const TMUX_FIELD_SEPARATOR: char = '\x1f';

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

    fn run_snapshot(&self, args: &[&str]) -> Result<Option<String>> {
        let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        let output = self.runner.run(&self.program, &args)?;
        if output.success {
            return Ok(Some(output.stdout));
        }
        if tmux_server_exited(&output.stderr) {
            return Ok(None);
        }
        require_success(&self.program, &args, output).map(Some)
    }

    fn run_owned(&self, args: Vec<String>) -> Result<String> {
        let output = self.runner.run(&self.program, &args)?;
        require_success(&self.program, &args, output)
    }

    fn run_owned_allow_server_exit(&self, args: Vec<String>) -> Result<String> {
        let output = self.runner.run(&self.program, &args)?;
        if !output.success && tmux_server_exited(&output.stderr) {
            return Ok(String::new());
        }
        require_success(&self.program, &args, output)
    }

    fn run_disowned_owned(&self, args: Vec<String>) -> Result<String> {
        let output = self.runner.run_disowned(&self.program, &args)?;
        require_success(&self.program, &args, output)
    }
}

impl<R: CommandRunner> MuxBackend for TmuxBackend<R> {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        let Some(sessions) = self.run_snapshot(&[
            "list-sessions",
            "-F",
            "#{session_id}\x1f#{session_name}\x1f#{session_attached}\x1f#{session_windows}\x1f#{pane_id}\x1f#{pane_current_path}\x1f#{pane_current_command}",
        ])? else {
            return Ok(MuxSnapshot::default());
        };
        let Some(panes) = self.run_snapshot(&[
            "list-panes",
            "-a",
            "-F",
            "#{session_id}\x1f#{window_id}\x1f#{window_index}\x1f#{window_name}\x1f#{window_active}\x1f#{pane_active}\x1f#{pane_id}\x1f#{pane_pb_state}\x1f#{pane_pb_progress}\x1f#{pane_current_path}\x1f#{pane_current_command}",
        ])? else {
            return Ok(MuxSnapshot::default());
        };
        parse_tmux_snapshot(&sessions, &panes)
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        match command {
            MuxCommand::ActivateWindow {
                session_id: _,
                window_id,
            } => {
                self.run_owned(vec!["select-window".into(), "-t".into(), window_id])?;
            }
            MuxCommand::CreateProjectSession { session_id, cwd }
            | MuxCommand::CreateWorktreeSession { session_id, cwd } => {
                self.run_disowned_owned(vec![
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
                self.run_owned_allow_server_exit(vec![
                    "kill-session".into(),
                    "-t".into(),
                    session_id,
                ])?;
            }
            MuxCommand::NewWindow { session_id, cwd } => {
                let mut args = vec!["new-window".to_owned(), "-t".to_owned(), session_id];
                if let Some(cwd) = cwd {
                    args.extend(["-c".to_owned(), cwd]);
                }
                self.run_owned(args)?;
            }
            MuxCommand::RenameWindow {
                session_id: _,
                window_id,
                name,
            } => {
                self.run_owned(vec!["rename-window".into(), "-t".into(), window_id, name])?;
            }
            MuxCommand::ActivateNextWindow { session_id } => {
                self.run_owned(vec!["next-window".into(), "-t".into(), session_id])?;
            }
            MuxCommand::ActivatePreviousWindow { session_id } => {
                self.run_owned(vec!["previous-window".into(), "-t".into(), session_id])?;
            }
            MuxCommand::ActivateLastWindow { session_id } => {
                self.run_owned(vec!["last-window".into(), "-t".into(), session_id])?;
            }
            MuxCommand::ActivateWindowIndex { session_id, index } => {
                self.run_owned(vec![
                    "select-window".into(),
                    "-t".into(),
                    format!("{session_id}:{index}"),
                ])?;
            }
            MuxCommand::MoveWindow {
                session_id,
                window_id,
                delta,
            } => {
                if delta != 0 {
                    self.run_owned(vec![
                        "select-window".into(),
                        "-t".into(),
                        window_id.unwrap_or(session_id),
                    ])?;
                }
                // Relative swap, following the moved window, matching tmux/rmux copy of the
                // active-window move behavior after selecting the requested source window.
                let target = if delta < 0 { "-1" } else { "+1" };
                for _ in 0..delta.unsigned_abs() {
                    self.run(&["swap-window", "-t", target])?;
                    self.run(&["select-window", "-t", target])?;
                }
            }
            MuxCommand::MoveWindowPreservingSelection {
                session_id: _,
                window_id,
                delta,
                selected_window_id,
            } => {
                if delta != 0 {
                    self.run_owned(vec!["select-window".into(), "-t".into(), window_id])?;
                }
                let target = if delta < 0 { "-1" } else { "+1" };
                for _ in 0..delta.unsigned_abs() {
                    self.run(&["swap-window", "-t", target])?;
                    self.run(&["select-window", "-t", target])?;
                }
                self.run_owned(vec![
                    "select-window".into(),
                    "-t".into(),
                    selected_window_id,
                ])?;
            }
            MuxCommand::SplitPane {
                session_id,
                direction,
                ..
            } => {
                let flag = match direction {
                    MuxSplitDirection::Right => "-h",
                    MuxSplitDirection::Down => "-v",
                };
                self.run_owned(vec![
                    "split-window".into(),
                    flag.into(),
                    "-t".into(),
                    session_id,
                ])?;
            }
            MuxCommand::SelectPane {
                session_id,
                direction,
            } => {
                let flag = match direction {
                    MuxDirection::Left => "-L",
                    MuxDirection::Down => "-D",
                    MuxDirection::Up => "-U",
                    MuxDirection::Right => "-R",
                };
                self.run_owned(vec![
                    "select-pane".into(),
                    "-t".into(),
                    session_id,
                    flag.into(),
                ])?;
            }
            MuxCommand::SelectNextPane { session_id } => {
                self.run_owned(vec![
                    "select-pane".into(),
                    "-t".into(),
                    format!("{session_id}:.+"),
                ])?;
            }
            MuxCommand::SelectPreviousPane { session_id } => {
                self.run_owned(vec![
                    "select-pane".into(),
                    "-t".into(),
                    format!("{session_id}:.-"),
                ])?;
            }
            MuxCommand::KillPane {
                session_id,
                pane_id,
            }
            | MuxCommand::ClosePane {
                session_id,
                pane_id,
            } => {
                self.run_owned_allow_server_exit(vec![
                    "kill-pane".into(),
                    "-t".into(),
                    pane_id.unwrap_or(session_id),
                ])?;
            }
            MuxCommand::TogglePaneZoom { session_id } => {
                self.run_owned(vec![
                    "resize-pane".into(),
                    "-Z".into(),
                    "-t".into(),
                    session_id,
                ])?;
            }
        }
        Ok(())
    }
}

fn tmux_server_exited(stderr: &str) -> bool {
    stderr.contains("no server running")
}

fn tmux_fields(line: &str, fixed_fields_before_tail: usize) -> Vec<String> {
    if line.contains(TMUX_FIELD_SEPARATOR) {
        return line
            .split(TMUX_FIELD_SEPARATOR)
            .map(str::to_owned)
            .collect();
    }
    if line.contains('\t') {
        return line.split('\t').map(str::to_owned).collect();
    }
    if line.contains("\\t") {
        return line.split("\\t").map(str::to_owned).collect();
    }
    underscore_joined_tmux_fields(line, fixed_fields_before_tail)
}

fn underscore_joined_tmux_fields(line: &str, fixed_fields_before_tail: usize) -> Vec<String> {
    let mut parts = line
        .splitn(fixed_fields_before_tail + 1, '_')
        .collect::<Vec<_>>();
    if parts.len() <= fixed_fields_before_tail {
        return vec![line.to_owned()];
    }
    let Some(tail) = parts.pop() else {
        return vec![line.to_owned()];
    };
    let Some((cwd, process)) = tail.rsplit_once('_') else {
        return vec![line.to_owned()];
    };
    let mut fields = parts.into_iter().map(str::to_owned).collect::<Vec<_>>();
    fields.push(cwd.to_owned());
    fields.push(process.to_owned());
    fields
}

fn parse_tmux_snapshot(sessions_output: &str, panes_output: &str) -> Result<MuxSnapshot> {
    let mut sessions = Vec::new();
    for line in sessions_output
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        let mut fields = tmux_fields(line, 5).into_iter();
        let id = fields.next().context("tmux snapshot missing session id")?;
        let name = fields
            .next()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| id.clone());
        let attached = fields.next().is_some_and(|value| value != "0");
        let _windows = fields.next();
        let pane_id = fields.next().filter(|value| !value.is_empty());
        let cwd = fields.next().filter(|value| !value.is_empty());
        let process = fields.next().filter(|value| !value.is_empty());
        sessions.push(MuxSession {
            id: id.clone(),
            name: name.clone(),
            active: attached,
            anchor: MuxPaneAnchor {
                session_id: id,
                pane_id,
                cwd,
                process,
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

/// tmux reports `hidden` for a pane with no progress, and an empty state for versions that
/// predate the format entirely; both mean "nothing to draw".
fn tmux_pane_progress(state: Option<String>, percent: Option<String>) -> Option<MuxWindowProgress> {
    let state = state.filter(|state| !state.is_empty() && state != "hidden")?;
    Some(MuxWindowProgress {
        percent: percent.and_then(|percent| percent.parse().ok()),
        state,
    })
}

fn furthest_along(
    current: Option<MuxWindowProgress>,
    candidate: Option<MuxWindowProgress>,
) -> Option<MuxWindowProgress> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(if candidate.percent > current.percent {
            candidate
        } else {
            current
        }),
        (current, candidate) => current.or(candidate),
    }
}

fn add_tmux_windows(sessions: &mut [MuxSession], panes_output: &str) -> Result<()> {
    for line in panes_output.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = tmux_fields(line, 9).into_iter();
        let Some(session_id) = fields.next().filter(|value| !value.is_empty()) else {
            continue;
        };
        let Some(window_id) = fields.next().filter(|value| !value.is_empty()) else {
            continue;
        };
        let Some(window_index) = fields.next().and_then(|value| value.parse().ok()) else {
            continue;
        };
        let Some(window_name) = fields.next() else {
            continue;
        };
        let window_active = fields.next().is_some_and(|value| value != "0");
        let pane_active = fields.next().is_some_and(|value| value != "0");
        let pane_id = fields.next().filter(|value| !value.is_empty());
        let progress = tmux_pane_progress(fields.next(), fields.next());
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
                    session_id: session_id.clone(),
                    pane_id: pane_id.clone(),
                    cwd: cwd.clone(),
                    process: process.clone(),
                };
            }
            // A window's bar stands for every pane in it, so the busiest pane wins.
            window.progress = furthest_along(window.progress.take(), progress);
            continue;
        }
        let anchor = MuxPaneAnchor {
            session_id,
            pane_id,
            cwd,
            process,
        };
        session.windows.push(MuxWindow {
            id: window_id,
            index: window_index,
            name: window_name,
            active: window_active,
            // tmux owns its own pane layout; bootty renders the single attach surface, so expose
            // just the attach anchor here.
            panes: vec![anchor.clone()],
            layout: None,
            anchor,
            progress,
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
    use crate::process::{CommandOutput, CommandRunner};

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct RecordedCall {
        disowned: bool,
        argv: Vec<String>,
    }

    impl RecordedCall {
        fn foreground<const N: usize>(argv: [&str; N]) -> Self {
            Self {
                disowned: false,
                argv: argv.into_iter().map(str::to_owned).collect(),
            }
        }

        fn disowned<const N: usize>(argv: [&str; N]) -> Self {
            Self {
                disowned: true,
                argv: argv.into_iter().map(str::to_owned).collect(),
            }
        }
    }
    #[derive(Clone, Default)]
    struct RecordingRunner {
        calls: Rc<RefCell<Vec<RecordedCall>>>,
        stdout: Rc<RefCell<VecDeque<String>>>,
        stderr: Rc<RefCell<VecDeque<String>>>,
        success: Rc<RefCell<VecDeque<bool>>>,
    }

    impl CommandRunner for RecordingRunner {
        fn run(&self, program: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
            self.record_call(program, args, false)
        }

        fn run_disowned(&self, program: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
            self.record_call(program, args, true)
        }
    }

    impl RecordingRunner {
        fn record_call(
            &self,
            program: &str,
            args: &[String],
            disowned: bool,
        ) -> anyhow::Result<CommandOutput> {
            let mut call = vec![program.to_owned()];
            call.extend(args.iter().cloned());
            self.calls.borrow_mut().push(RecordedCall {
                disowned,
                argv: call,
            });
            Ok(CommandOutput {
                success: self.success.borrow_mut().pop_front().unwrap_or(true),
                stdout: self.stdout.borrow_mut().pop_front().unwrap_or_default(),
                stderr: self.stderr.borrow_mut().pop_front().unwrap_or_default(),
            })
        }
    }

    #[test]
    fn tmux_adapter_translates_lifecycle_commands() {
        let runner = RecordingRunner::default();
        let calls = runner.calls.clone();
        let mut backend = TmuxBackend::with_runner("tmux", runner);

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
            [
                RecordedCall::foreground(["tmux", "select-window", "-t", "@2"]),
                RecordedCall::disowned(["tmux", "new-session", "-d", "-s", "proj", "-c", "/repo"]),
                RecordedCall::foreground(["tmux", "rename-session", "-t", "proj", "next"]),
                RecordedCall::foreground(["tmux", "kill-session", "-t", "next"]),
            ]
            .as_slice()
        );
    }

    #[test]
    fn tmux_close_cleanup_tolerates_server_already_exited() {
        let runner = RecordingRunner {
            success: Rc::new(RefCell::new(VecDeque::from([false]))),
            stderr: Rc::new(RefCell::new(VecDeque::from([
                "no server running on /tmp/tmux-501/default".to_owned(),
            ]))),
            ..Default::default()
        };
        let mut backend = TmuxBackend::with_runner("tmux", runner);

        backend
            .execute(MuxCommand::ClosePane {
                session_id: "$1".to_owned(),
                pane_id: None,
            })
            .unwrap();
    }

    #[test]
    fn tmux_close_pane_targets_the_requested_pane() {
        let runner = RecordingRunner::default();
        let calls = runner.calls.clone();
        let mut backend = TmuxBackend::with_runner("tmux", runner);

        backend
            .execute(MuxCommand::ClosePane {
                session_id: "$1".to_owned(),
                pane_id: Some("%9".to_owned()),
            })
            .unwrap();

        assert_eq!(
            calls.borrow().as_slice(),
            [RecordedCall::foreground(["tmux", "kill-pane", "-t", "%9"])].as_slice()
        );
    }

    #[test]
    fn tmux_adapter_translates_window_and_pane_navigation() {
        let runner = RecordingRunner::default();
        let calls = runner.calls.clone();
        let mut backend = TmuxBackend::with_runner("tmux", runner);

        backend
            .execute(MuxCommand::NewWindow {
                session_id: "$1".to_owned(),
                cwd: Some("/repo".to_owned()),
            })
            .unwrap();
        for command in [
            MuxCommand::ActivateWindowIndex {
                session_id: "$1".to_owned(),
                index: 3,
            },
            MuxCommand::ActivateNextWindow {
                session_id: "$1".to_owned(),
            },
            MuxCommand::SelectPane {
                session_id: "$1".to_owned(),
                direction: MuxDirection::Left,
            },
            MuxCommand::TogglePaneZoom {
                session_id: "$1".to_owned(),
            },
        ] {
            backend.execute(command).unwrap();
        }

        assert_eq!(
            calls.borrow().as_slice(),
            [
                RecordedCall::foreground(["tmux", "new-window", "-t", "$1", "-c", "/repo"]),
                RecordedCall::foreground(["tmux", "select-window", "-t", "$1:3"]),
                RecordedCall::foreground(["tmux", "next-window", "-t", "$1"]),
                RecordedCall::foreground(["tmux", "select-pane", "-t", "$1", "-L"]),
                RecordedCall::foreground(["tmux", "resize-pane", "-Z", "-t", "$1"]),
            ]
            .as_slice()
        );
    }

    #[test]
    fn tmux_adapter_moves_target_window_relative_and_follows_it() {
        let runner = RecordingRunner::default();
        let calls = runner.calls.clone();
        let mut backend = TmuxBackend::with_runner("tmux", runner);

        backend
            .execute(MuxCommand::MoveWindow {
                session_id: "$1".to_owned(),
                window_id: Some("@2".to_owned()),
                delta: -2,
            })
            .unwrap();

        assert_eq!(
            calls.borrow().as_slice(),
            [
                RecordedCall::foreground(["tmux", "select-window", "-t", "@2"]),
                RecordedCall::foreground(["tmux", "swap-window", "-t", "-1"]),
                RecordedCall::foreground(["tmux", "select-window", "-t", "-1"]),
                RecordedCall::foreground(["tmux", "swap-window", "-t", "-1"]),
                RecordedCall::foreground(["tmux", "select-window", "-t", "-1"]),
            ]
            .as_slice()
        );
    }

    #[test]
    fn tmux_context_move_restores_the_previously_active_window() {
        let runner = RecordingRunner::default();
        let calls = runner.calls.clone();
        let mut backend = TmuxBackend::with_runner("tmux", runner);

        backend
            .execute(MuxCommand::MoveWindowPreservingSelection {
                session_id: "$1".to_owned(),
                window_id: "@2".to_owned(),
                delta: 1,
                selected_window_id: "@3".to_owned(),
            })
            .unwrap();

        assert_eq!(
            calls.borrow().as_slice(),
            [
                RecordedCall::foreground(["tmux", "select-window", "-t", "@2"]),
                RecordedCall::foreground(["tmux", "swap-window", "-t", "+1"]),
                RecordedCall::foreground(["tmux", "select-window", "-t", "+1"]),
                RecordedCall::foreground(["tmux", "select-window", "-t", "@3"]),
            ]
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
            ..Default::default()
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

    #[test]
    fn tmux_snapshot_returns_empty_when_server_has_exited() {
        let runner = RecordingRunner {
            success: Rc::new(RefCell::new(VecDeque::from([false]))),
            stderr: Rc::new(RefCell::new(VecDeque::from([
                "no server running on /tmp/tmux-501/default".to_owned(),
            ]))),
            ..Default::default()
        };
        let calls = runner.calls.clone();
        let backend = TmuxBackend::with_runner("tmux", runner);

        let snapshot = backend.snapshot().unwrap();

        assert_eq!(snapshot, MuxSnapshot::default());
        assert_eq!(calls.borrow().len(), 1);
    }

    #[test]
    fn tmux_snapshot_falls_back_to_id_when_session_name_is_missing() {
        let snapshot = parse_tmux_snapshot("$1\n", "").unwrap();

        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(snapshot.sessions[0].id, "$1");
        assert_eq!(snapshot.sessions[0].name, "$1");
    }

    #[test]
    fn tmux_snapshot_accepts_unit_separator_delimiters() {
        let snapshot = parse_tmux_snapshot(
            "$1\x1fboo\x1f1\x1f5\x1f%3\x1f/Users/luan/src/boo\x1fnode\n",
            "$1\x1f@3\x1f1\x1fai\x1f1\x1f1\x1f%3\x1f/Users/luan/src/boo\x1fnode\n",
        )
        .unwrap();

        assert_eq!(snapshot.active_session_id.as_deref(), Some("$1"));
        assert_eq!(snapshot.sessions[0].id, "$1");
        assert_eq!(snapshot.sessions[0].name, "boo");
        assert_eq!(snapshot.sessions[0].anchor.pane_id.as_deref(), Some("%3"));
        assert_eq!(snapshot.sessions[0].active_window_id.as_deref(), Some("@3"));
    }

    #[test]
    fn tmux_snapshot_recovers_underscore_joined_rows() {
        let snapshot = parse_tmux_snapshot(
            "$2_agents_0_3_%34_/Users/luan/src/agents_node\n",
            "$2_@28_1_ai_1_1_%34_normal_42_/Users/luan/src/agents_node\n",
        )
        .unwrap();

        assert_eq!(snapshot.active_session_id, None);
        assert_eq!(snapshot.sessions[0].id, "$2");
        assert_eq!(snapshot.sessions[0].name, "agents");
        assert_eq!(snapshot.sessions[0].anchor.pane_id.as_deref(), Some("%34"));
        assert_eq!(
            snapshot.sessions[0].anchor.cwd.as_deref(),
            Some("/Users/luan/src/agents")
        );
        assert_eq!(snapshot.sessions[0].anchor.process.as_deref(), Some("node"));
        assert_eq!(
            snapshot.sessions[0].active_window_id.as_deref(),
            Some("@28")
        );
        assert_eq!(snapshot.sessions[0].windows[0].name, "ai");
    }

    #[test]
    fn tmux_snapshot_accepts_literal_backslash_t_delimiters() {
        let snapshot = parse_tmux_snapshot(
            "$1\\tboo\\t1\\t5\\t%3\\t/Users/luan/src/boo\\tnode\n",
            "$1\\t@3\\t1\\tai\\t1\\t1\\t%3\\t/Users/luan/src/boo\\tnode\n",
        )
        .unwrap();

        assert_eq!(snapshot.active_session_id.as_deref(), Some("$1"));
        assert_eq!(snapshot.sessions[0].id, "$1");
        assert_eq!(snapshot.sessions[0].name, "boo");
        assert_eq!(snapshot.sessions[0].anchor.pane_id.as_deref(), Some("%3"));
        assert_eq!(snapshot.sessions[0].active_window_id.as_deref(), Some("@3"));
    }

    #[test]
    fn tmux_snapshot_reads_window_progress_from_the_busiest_pane() {
        let sessions = "$1\x1fboo\x1f1\x1f2\x1f%1\x1f/repo\x1fnode\n";
        let panes = concat!(
            "$1\x1f@1\x1f1\x1fbuild\x1f1\x1f1\x1f%1\x1fnormal\x1f12\x1f/repo\x1fnode\n",
            "$1\x1f@1\x1f1\x1fbuild\x1f1\x1f0\x1f%2\x1fnormal\x1f80\x1f/repo\x1fnode\n",
            "$1\x1f@2\x1f2\x1fidle\x1f0\x1f1\x1f%3\x1fhidden\x1f0\x1f/repo\x1ffish\n",
        );

        let snapshot = parse_tmux_snapshot(sessions, panes).unwrap();

        let windows = &snapshot.sessions[0].windows;
        assert_eq!(
            windows[0].progress,
            Some(MuxWindowProgress {
                state: "normal".to_owned(),
                percent: Some(80),
            })
        );
        assert_eq!(windows[1].progress, None);
        // Field order drifting between the query and the parser would land the state string in
        // cwd; check the tail still resolves.
        assert_eq!(windows[0].anchor.cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn tmux_snapshot_skips_incomplete_pane_rows() {
        let snapshot = parse_tmux_snapshot("$1\talpha\t1\t1\n", "$1\n").unwrap();

        assert_eq!(snapshot.sessions.len(), 1);
        assert!(snapshot.sessions[0].windows.is_empty());
    }
}
