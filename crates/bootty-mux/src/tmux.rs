use anyhow::{Context, Result};

#[cfg(not(feature = "app"))]
use super::process::SystemCommandRunner;
#[cfg(feature = "app")]
use super::{
    backend::MuxBackend,
    capability::{BindingCapabilityDescriptor, BindingOperation},
    controller::MuxScope,
    tmux_control::TmuxControlRunner,
};
use super::{
    command::{MuxCommand, MuxDirection, MuxSplitDirection},
    process::{CommandRunner, require_success},
    snapshot::{MuxPaneAnchor, MuxSession, MuxSnapshot, MuxWindow, MuxWindowProgress},
};

const TMUX_FIELD_SEPARATOR: char = '\x1f';
/// Line tags for the combined session/pane snapshot. Sessions and panes come from one tmux
/// invocation, so each line says which list it belongs to.
const TMUX_SESSION_LINE_TAG: char = 's';
const TMUX_PANE_LINE_TAG: char = 'p';

#[cfg(feature = "app")]
pub(crate) fn local_server_args(identity: bootty_identity::ApplicationIdentity) -> Vec<String> {
    match identity {
        bootty_identity::ApplicationIdentity::Production => Vec::new(),
        bootty_identity::ApplicationIdentity::Development => {
            vec!["-L".to_owned(), identity.namespace().to_owned()]
        }
    }
}

#[cfg(feature = "app")]
pub type DefaultTmuxRunner = TmuxControlRunner;
#[cfg(not(feature = "app"))]
pub type DefaultTmuxRunner = SystemCommandRunner;

#[derive(Clone, Debug)]
pub struct TmuxBackend<R = DefaultTmuxRunner> {
    program: String,
    runner: R,
}

#[cfg(feature = "app")]
impl TmuxBackend<DefaultTmuxRunner> {
    pub fn new() -> Self {
        Self::with_runner("tmux", TmuxControlRunner::default())
    }

    pub fn for_identity(identity: bootty_identity::ApplicationIdentity) -> Self {
        Self::with_runner("tmux", TmuxControlRunner::for_identity(identity))
    }
}

#[cfg(not(feature = "app"))]
impl TmuxBackend<DefaultTmuxRunner> {
    pub fn new() -> Self {
        Self::with_runner("tmux", SystemCommandRunner)
    }
}

impl Default for TmuxBackend<DefaultTmuxRunner> {
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

impl<R: CommandRunner> TmuxBackend<R> {
    pub fn snapshot(&self) -> Result<MuxSnapshot> {
        // One tmux process for both lists: the snapshot polls several times a second, and a
        // second invocation doubled that process churn for no extra information.
        let Some(combined) = self.run_snapshot(&[
            "list-sessions",
            "-F",
            "s\x1f#{session_id}\x1f#{session_name}\x1f#{session_attached}\x1f#{session_windows}\x1f#{pane_id}\x1f#{pane_pid}\x1f#{pane_current_path}\x1f#{pane_current_command}",
            ";",
            "list-panes",
            "-a",
            "-F",
            "p\x1f#{session_id}\x1f#{window_id}\x1f#{window_index}\x1f#{window_name}\x1f#{window_active}\x1f#{pane_active}\x1f#{pane_id}\x1f#{pane_pb_state}\x1f#{pane_pb_progress}\x1f#{pane_current_path}\x1f#{pane_current_command}",
        ])? else {
            return Ok(MuxSnapshot::default());
        };
        let (sessions, panes) = split_tagged_snapshot(&combined);
        parse_tmux_snapshot(&sessions, &panes)
    }

    pub fn execute(&mut self, command: MuxCommand) -> Result<()> {
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
                pane_id,
                direction,
            } => {
                let flag = match direction {
                    MuxSplitDirection::Right => "-h",
                    MuxSplitDirection::Down => "-v",
                };
                self.run_owned(vec![
                    "split-window".into(),
                    flag.into(),
                    "-t".into(),
                    pane_id.unwrap_or(session_id),
                ])?;
            }
            MuxCommand::SelectPane {
                session_id,
                window_id,
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
                    window_id.unwrap_or(session_id),
                    flag.into(),
                ])?;
            }
            MuxCommand::SelectNextPane {
                session_id,
                window_id,
            } => {
                let target = window_id.map_or_else(
                    || format!("{session_id}:.+"),
                    |window_id| format!("{window_id}.+"),
                );
                self.run_owned(vec!["select-pane".into(), "-t".into(), target])?;
            }
            MuxCommand::SelectPreviousPane {
                session_id,
                window_id,
            } => {
                let target = window_id.map_or_else(
                    || format!("{session_id}:.-"),
                    |window_id| format!("{window_id}.-"),
                );
                self.run_owned(vec!["select-pane".into(), "-t".into(), target])?;
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
            MuxCommand::TogglePaneZoom {
                session_id,
                pane_id,
            } => {
                self.run_owned(vec![
                    "resize-pane".into(),
                    "-Z".into(),
                    "-t".into(),
                    pane_id.unwrap_or(session_id),
                ])?;
            }
        }
        Ok(())
    }
}

#[cfg(feature = "app")]
impl<R: CommandRunner> MuxBackend for TmuxBackend<R> {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        TmuxBackend::snapshot(self)
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        TmuxBackend::execute(self, command)
    }

    fn capabilities(&self, scope: MuxScope) -> BindingCapabilityDescriptor {
        tmux_capabilities(scope)
    }
}

#[cfg(feature = "app")]
pub(crate) fn tmux_capabilities(scope: MuxScope) -> BindingCapabilityDescriptor {
    BindingCapabilityDescriptor::new(
        scope,
        [
            BindingOperation::ActivateWindow,
            BindingOperation::CreateWindow,
            BindingOperation::RenameWindow,
            BindingOperation::NavigateWindow,
            BindingOperation::MoveWindow,
            BindingOperation::SplitPane,
            BindingOperation::NavigatePane,
            BindingOperation::ClosePane,
            BindingOperation::TogglePaneZoom,
            BindingOperation::CreateProjectSession,
            BindingOperation::CreateWorktreeSession,
            BindingOperation::RenameSession,
            BindingOperation::DitchSession,
        ],
    )
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

/// Split the tagged output of the combined `list-sessions ; list-panes` call back into the two
/// listings the parsers expect. Untagged lines belong to neither list and are dropped.
fn split_tagged_snapshot(combined: &str) -> (String, String) {
    let mut sessions = String::new();
    let mut panes = String::new();
    for line in combined.lines() {
        let (target, rest) = if let Some(rest) = strip_snapshot_tag(line, TMUX_SESSION_LINE_TAG) {
            (&mut sessions, rest)
        } else if let Some(rest) = strip_snapshot_tag(line, TMUX_PANE_LINE_TAG) {
            (&mut panes, rest)
        } else {
            continue;
        };
        target.push_str(rest);
        target.push('\n');
    }
    (sessions, panes)
}

/// Drop a line's list tag and the separator after it. The separator forms match the ones
/// [`tmux_fields`] accepts, since a tmux build that renders `\x1f` as something else does so for
/// the tag too.
fn strip_snapshot_tag(line: &str, tag: char) -> Option<&str> {
    let tagged = line.strip_prefix(tag)?;
    [TMUX_FIELD_SEPARATOR, '\t', '_']
        .into_iter()
        .find_map(|separator| tagged.strip_prefix(separator))
        .or_else(|| tagged.strip_prefix("\\t"))
}

fn parse_tmux_snapshot(sessions_output: &str, panes_output: &str) -> Result<MuxSnapshot> {
    let mut sessions = Vec::new();
    for line in sessions_output
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        let mut fields = tmux_fields(line, 6).into_iter();
        let id = fields.next().context("tmux snapshot missing session id")?;
        let name = fields
            .next()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| id.clone());
        let attached = fields.next().is_some_and(|value| value != "0");
        let _windows = fields.next();
        let pane_id = fields.next().filter(|value| !value.is_empty());
        let pane_pid = fields.next().and_then(|value| value.parse().ok());
        let cwd = fields.next().filter(|value| !value.is_empty());
        let process = fields.next().filter(|value| !value.is_empty());
        sessions.push(MuxSession {
            id: id.clone(),
            name: name.clone(),
            active: attached,
            anchor: MuxPaneAnchor {
                session_id: id,
                pane_id,
                pane_pid,
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
                    pane_pid: None,
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
            pane_pid: None,
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
