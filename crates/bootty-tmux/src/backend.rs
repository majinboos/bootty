use anyhow::{Context, Result};

#[cfg(feature = "app")]
use std::{collections::HashMap, process::Command};

#[cfg(feature = "app")]
use crate::control::TmuxControlRunner;
#[cfg(not(feature = "app"))]
use bootty_mux::process::SystemCommandRunner;
use bootty_mux::{
    backend::MuxBackend,
    command::{MuxCommand, MuxDirection, MuxSplitDirection},
    process::{CommandRunner, require_success},
    snapshot::{MuxPaneAnchor, MuxSession, MuxSnapshot, MuxWindow, MuxWindowProgress},
};
#[cfg(feature = "app")]
use bootty_mux::{
    capability::{BindingCapabilityDescriptor, BindingOperation},
    controller::MuxScope,
    terminal::{
        AttachLaunch, BackendPanePolicy, PaneLayoutResizeRequest, PaneStartRequest,
        ScopedMuxPaneTarget, TerminalRuntime, resolve_launch_program, start_attach_terminal,
    },
};
#[cfg(feature = "app")]
use bootty_remote::ssh::SshRemote;

const TMUX_FIELD_SEPARATOR: char = '\x1f';
/// Line tags for the combined session/pane snapshot. Sessions and panes come from one tmux
/// invocation, so each line says which list it belongs to.
const TMUX_SESSION_LINE_TAG: char = 's';
const TMUX_PANE_LINE_TAG: char = 'p';

#[cfg(feature = "app")]
pub fn local_server_args(identity: bootty_identity::ApplicationIdentity) -> Vec<String> {
    match identity {
        bootty_identity::ApplicationIdentity::Production => Vec::new(),
        bootty_identity::ApplicationIdentity::Development => {
            vec!["-L".to_owned(), identity.namespace().to_owned()]
        }
    }
}

#[cfg(feature = "app")]
const TMUX_CLIENT_FEATURES: &str =
    "256,RGB,clipboard,focus,hyperlinks,overline,strikethrough,sync,title";

#[cfg(feature = "app")]
struct TmuxOptionValue {
    value: String,
    local: bool,
}

#[cfg(feature = "app")]
pub struct TmuxPanePolicy {
    remote: Option<SshRemote>,
    status_hidden_sessions: Vec<String>,
    passthrough_all_panes: HashMap<String, TmuxOptionValue>,
}

#[cfg(feature = "app")]
impl TmuxPanePolicy {
    pub fn new(remote: Option<SshRemote>) -> Self {
        Self {
            remote,
            status_hidden_sessions: Vec::new(),
            passthrough_all_panes: HashMap::new(),
        }
    }

    fn sync_passthrough_override(&mut self, target: Option<&ScopedMuxPaneTarget>) {
        let Some(pane_id) = target.map(ScopedMuxPaneTarget::input_selector) else {
            self.restore_passthrough_overrides();
            return;
        };
        if self.passthrough_all_panes.contains_key(pane_id) {
            return;
        }
        if let Ok(previous) = take_pane_allow_passthrough(self.remote.as_ref(), pane_id) {
            self.passthrough_all_panes
                .insert(pane_id.to_owned(), previous);
        }
    }

    fn restore_passthrough_overrides(&mut self) {
        for (pane_id, previous) in self.passthrough_all_panes.drain() {
            let _ = restore_pane_allow_passthrough(self.remote.as_ref(), &pane_id, &previous);
        }
    }

    fn sync_status_bar(&mut self, target: Option<&ScopedMuxPaneTarget>, hide: bool) {
        let Some(session) = target.filter(|_| hide).map(ScopedMuxPaneTarget::session_id) else {
            self.restore_status_bars();
            return;
        };
        if self
            .status_hidden_sessions
            .iter()
            .any(|hidden| hidden == session)
        {
            return;
        }
        if set_session_status_hidden(self.remote.as_ref(), session, true).is_ok() {
            self.status_hidden_sessions.push(session.to_owned());
        }
    }

    fn restore_status_bars(&mut self) {
        for session in self.status_hidden_sessions.drain(..) {
            let _ = set_session_status_hidden(self.remote.as_ref(), &session, false);
        }
    }
}

#[cfg(feature = "app")]
impl BackendPanePolicy for TmuxPanePolicy {
    fn remote_target(&self) -> Option<&bootty_mux_model::SshTarget> {
        self.remote.as_ref().map(SshRemote::target)
    }

    fn start_terminal(
        &mut self,
        request: PaneStartRequest<'_>,
    ) -> Result<Option<Box<dyn TerminalRuntime>>> {
        let identity = if self.remote.is_some() {
            bootty_identity::ApplicationIdentity::Production
        } else {
            bootty_identity::ApplicationIdentity::for_process()
        };
        let mut args = local_server_args(identity);
        args.extend([
            "-T".to_owned(),
            TMUX_CLIENT_FEATURES.to_owned(),
            "attach-session".to_owned(),
            "-t".to_owned(),
            request.target.session_id().to_owned(),
        ]);
        let (program, args, remote) = match &self.remote {
            Some(remote) => {
                let (program, args) = remote.proxy_tty_command("tmux", &args)?;
                (program, args, true)
            }
            None => ("tmux".to_owned(), args, false),
        };
        start_attach_terminal(
            request,
            AttachLaunch {
                program,
                args,
                env_remove: vec!["TMUX".to_owned()],
                env: Vec::new(),
                remote,
            },
        )
        .map(Some)
    }

    fn sync_target(&mut self, target: Option<&ScopedMuxPaneTarget>, hide_tmux_status: bool) {
        self.sync_passthrough_override(target);
        self.sync_status_bar(target, hide_tmux_status);
    }

    fn set_layout_window(&mut self, _window_id: Option<&str>) {}

    fn resize_layout_window(&mut self, _request: PaneLayoutResizeRequest<'_>) -> Result<bool> {
        Ok(false)
    }

    fn deactivate(&mut self) {
        self.restore_passthrough_overrides();
        self.restore_status_bars();
    }
}

#[cfg(feature = "app")]
fn take_pane_allow_passthrough(
    remote: Option<&SshRemote>,
    pane_id: &str,
) -> Result<TmuxOptionValue> {
    let stdout = run_tmux(
        remote,
        &[
            "show-options",
            "-p",
            "-t",
            pane_id,
            "allow-passthrough",
            ";",
            "show-options",
            "-g",
            "allow-passthrough",
            ";",
            "set-option",
            "-p",
            "-t",
            pane_id,
            "allow-passthrough",
            "all",
        ],
        "allow-passthrough read-and-set",
    )?;
    parse_allow_passthrough(&stdout)
        .ok_or_else(|| anyhow::anyhow!("tmux reported no allow-passthrough value"))
}

#[cfg(feature = "app")]
fn run_tmux(remote: Option<&SshRemote>, args: &[&str], what: &str) -> Result<String> {
    let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    let (program, args) = match remote {
        Some(remote) => remote.command("tmux", &args),
        None => ("tmux".to_owned(), args),
    };
    let output = Command::new(resolve_launch_program(&program)?)
        .args(&args)
        .env_remove("TMUX")
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "tmux {what} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(feature = "app")]
fn parse_allow_passthrough(stdout: &str) -> Option<TmuxOptionValue> {
    let values: Vec<&str> = stdout
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .collect();
    Some(TmuxOptionValue {
        value: (*values.first()?).to_owned(),
        local: values.len() > 1,
    })
}

#[cfg(feature = "app")]
fn restore_pane_allow_passthrough(
    remote: Option<&SshRemote>,
    pane_id: &str,
    previous: &TmuxOptionValue,
) -> Result<()> {
    if previous.local {
        return set_pane_allow_passthrough(remote, pane_id, &previous.value);
    }
    run_tmux(
        remote,
        &["set-option", "-u", "-p", "-t", pane_id, "allow-passthrough"],
        "unset-option allow-passthrough",
    )
    .map(|_| ())
}

#[cfg(feature = "app")]
fn set_pane_allow_passthrough(
    remote: Option<&SshRemote>,
    pane_id: &str,
    value: &str,
) -> Result<()> {
    run_tmux(
        remote,
        &[
            "set-option",
            "-p",
            "-t",
            pane_id,
            "allow-passthrough",
            value,
        ],
        "set-option allow-passthrough",
    )
    .map(|_| ())
}

#[cfg(feature = "app")]
fn set_session_status_hidden(
    remote: Option<&SshRemote>,
    session_id: &str,
    hidden: bool,
) -> Result<()> {
    let args: &[&str] = if hidden {
        &["set-option", "-t", session_id, "status", "off"]
    } else {
        &["set-option", "-u", "-t", session_id, "status"]
    };
    run_tmux(remote, args, "set-option status").map(|_| ())
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
    fn run(&self, args: &[&str]) -> Result<()> {
        let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        let output = self.runner.run(&self.program, &args)?;
        require_success(&self.program, &args, output).map(|_| ())
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

    fn run_owned(&self, args: &[String]) -> Result<()> {
        let output = self.runner.run(&self.program, args)?;
        require_success(&self.program, args, output).map(|_| ())
    }

    fn run_owned_allow_server_exit(&self, args: &[String]) -> Result<()> {
        let output = self.runner.run(&self.program, args)?;
        if !output.success && tmux_server_exited(&output.stderr) {
            return Ok(());
        }
        require_success(&self.program, args, output).map(|_| ())
    }

    fn run_disowned_owned(&self, args: &[String]) -> Result<()> {
        let output = self.runner.run_disowned(&self.program, args)?;
        require_success(&self.program, args, output).map(|_| ())
    }

    fn move_window(&self, window_id: String, delta: i32) -> Result<()> {
        if delta != 0 {
            self.run_owned(&["select-window".into(), "-t".into(), window_id])?;
        }
        let target = if delta < 0 { "-1" } else { "+1" };
        for _ in 0..delta.unsigned_abs() {
            self.run(&["swap-window", "-t", target])?;
            self.run(&["select-window", "-t", target])?;
        }
        Ok(())
    }

    fn select_relative_pane(
        &self,
        session_id: &str,
        window_id: Option<String>,
        suffix: &str,
    ) -> Result<()> {
        let target = window_id.map_or_else(
            || format!("{session_id}:{suffix}"),
            |window_id| format!("{window_id}{suffix}"),
        );
        self.run_owned(&["select-pane".into(), "-t".into(), target])?;
        Ok(())
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
            MuxCommand::ActivateWindow { window_id, .. } => {
                self.run_owned(&["select-window".into(), "-t".into(), window_id])?;
            }
            MuxCommand::CreateProjectSession { session_id, cwd }
            | MuxCommand::CreateWorktreeSession { session_id, cwd } => {
                self.run_disowned_owned(&[
                    "new-session".into(),
                    "-d".into(),
                    "-s".into(),
                    session_id,
                    "-c".into(),
                    cwd,
                ])?;
            }
            MuxCommand::RenameSession { session_id, name } => {
                self.run_owned(&["rename-session".into(), "-t".into(), session_id, name])?;
            }
            MuxCommand::DitchSession { session_id } => {
                self.run_owned_allow_server_exit(&[
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
                self.run_owned(&args)?;
            }
            MuxCommand::RenameWindow {
                window_id, name, ..
            } => {
                self.run_owned(&["rename-window".into(), "-t".into(), window_id, name])?;
            }
            MuxCommand::ActivateNextWindow { session_id } => {
                self.run_owned(&["next-window".into(), "-t".into(), session_id])?;
            }
            MuxCommand::ActivatePreviousWindow { session_id } => {
                self.run_owned(&["previous-window".into(), "-t".into(), session_id])?;
            }
            MuxCommand::ActivateLastWindow { session_id } => {
                self.run_owned(&["last-window".into(), "-t".into(), session_id])?;
            }
            MuxCommand::ActivateWindowIndex { session_id, index } => {
                self.run_owned(&[
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
                self.move_window(window_id.unwrap_or(session_id), delta)?;
            }
            MuxCommand::MoveWindowPreservingSelection {
                window_id,
                delta,
                selected_window_id,
                ..
            } => {
                self.move_window(window_id, delta)?;
                self.run_owned(&["select-window".into(), "-t".into(), selected_window_id])?;
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
                self.run_owned(&[
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
                self.run_owned(&[
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
                self.select_relative_pane(&session_id, window_id, ".+")?;
            }
            MuxCommand::SelectPreviousPane {
                session_id,
                window_id,
            } => {
                self.select_relative_pane(&session_id, window_id, ".-")?;
            }
            MuxCommand::KillPane {
                session_id,
                pane_id,
            }
            | MuxCommand::ClosePane {
                session_id,
                pane_id,
            } => {
                self.run_owned_allow_server_exit(&[
                    "kill-pane".into(),
                    "-t".into(),
                    pane_id.unwrap_or(session_id),
                ])?;
            }
            MuxCommand::TogglePaneZoom {
                session_id,
                pane_id,
            } => {
                self.run_owned(&[
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

impl<R: CommandRunner> MuxBackend for TmuxBackend<R> {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        TmuxBackend::snapshot(self)
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        TmuxBackend::execute(self, command)
    }
}

#[cfg(feature = "app")]
pub fn tmux_capabilities(scope: MuxScope) -> BindingCapabilityDescriptor {
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
    if let Some(separator) = ["\x1f", "\t", "\\t"]
        .into_iter()
        .find(|separator| line.contains(separator))
    {
        return line.split(separator).map(str::to_owned).collect();
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
    let Some((cwd, process)) = parts.pop().and_then(|tail| tail.rsplit_once('_')) else {
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

fn nonempty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn parse_tmux_snapshot(session_listing: &str, pane_listing: &str) -> Result<MuxSnapshot> {
    let mut sessions = Vec::new();
    for line in session_listing
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        let mut fields = tmux_fields(line, 6).into_iter();
        let id = fields.next().context("tmux snapshot missing session id")?;
        let name = fields
            .next()
            .and_then(nonempty)
            .unwrap_or_else(|| id.clone());
        let attached = fields.next().is_some_and(|value| value != "0");
        let _windows = fields.next();
        let pane_id = fields.next().and_then(nonempty);
        let pane_process_id = fields.next().and_then(|value| value.parse().ok());
        let cwd = fields.next().and_then(nonempty);
        let process = fields.next().and_then(nonempty);
        sessions.push(MuxSession {
            id: id.clone(),
            name,
            active: attached,
            anchor: MuxPaneAnchor {
                session_id: id,
                pane_id,
                pane_pid: pane_process_id,
                cwd,
                process,
            },
            active_window_id: None,
            windows: Vec::new(),
        });
    }
    add_tmux_windows(&mut sessions, pane_listing);

    Ok(MuxSnapshot {
        active_session_id: sessions
            .iter()
            .find(|session| session.active)
            .map(|session| session.id.clone()),
        sessions,
        ..MuxSnapshot::default()
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

fn add_tmux_windows(sessions: &mut [MuxSession], pane_listing: &str) {
    for line in pane_listing.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = tmux_fields(line, 9).into_iter();
        let Some(session_id) = fields.next().and_then(nonempty) else {
            continue;
        };
        let Some(window_id) = fields.next().and_then(nonempty) else {
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
        let pane_id = fields.next().and_then(nonempty);
        let progress = tmux_pane_progress(fields.next(), fields.next());
        let cwd = fields.next().and_then(nonempty);
        let process = fields.next().and_then(nonempty);

        let Some(session) = sessions.iter_mut().find(|session| session.id == session_id) else {
            continue;
        };
        if window_active {
            session.active_window_id = Some(window_id.clone());
        }
        if let Some(window) = session
            .windows
            .iter_mut()
            .find(|window| window.id == window_id)
        {
            if pane_active || window.anchor.pane_id.is_none() {
                window.anchor = MuxPaneAnchor {
                    session_id,
                    pane_id,
                    pane_pid: None,
                    cwd,
                    process,
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
}
