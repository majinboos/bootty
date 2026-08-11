use anyhow::{Context, Result};
use std::collections::BTreeMap;

#[cfg(not(feature = "app"))]
use super::process::SystemCommandRunner;
#[cfg(feature = "app")]
use super::{
    backend::{MuxBackend, MuxEvent, MuxEventCapability, MuxScopedExecutionPrecondition},
    capability::{
        BindingCapabilityDescriptor, BindingOperation, BindingOperationAvailability,
        BindingOperationOutcome,
    },
    controller::MuxScope,
    tmux_control::TmuxControlRunner,
};
use super::{
    command::{
        MuxCommand, MuxDirection, MuxPaneLaunch, MuxPaneLaunchPlan, MuxPaneResize,
        MuxSessionLaunchPlan, MuxSplitDirection,
    },
    operation::{
        MuxAllocatedResources, MuxAllocatedWindow, MuxBackendCommandCompletion,
        MuxBackendOperationError, MuxEventTarget,
    },
    process::{CommandRunner, require_success},
    snapshot::{MuxPaneAnchor, MuxSession, MuxSnapshot, MuxWindow, MuxWindowProgress},
};

const TMUX_FIELD_SEPARATOR: char = '\x1f';
/// Line tags for the combined session/pane snapshot. Sessions and panes come from one tmux
/// invocation, so each line says which list it belongs to.
#[cfg(feature = "app")]
const TMUX_STALE_TARGET_MARKER: &str = "bootty-stale-target";
const TMUX_SESSION_LINE_TAG: char = 's';
const TMUX_PANE_LINE_TAG: char = 'p';

#[derive(Debug)]
struct TmuxLaunchTarget {
    window_id: String,
    pane_id: String,
}

/// Identities tmux returned while creating one session, before the caller observes its next
/// snapshot. Keeping these facts transaction-local avoids inferring recursive order from tmux's
/// intentionally flat attach snapshot.
#[derive(Debug)]
struct TmuxLaunchAllocation {
    session_id: String,
    windows: Vec<TmuxLaunchWindow>,
}

#[derive(Debug)]
struct TmuxLaunchWindow {
    window_id: String,
    pane_ids: Vec<String>,
}

#[derive(Debug)]
struct TmuxLaunchSessionTarget {
    session_id: String,
    target: TmuxLaunchTarget,
}

#[cfg(feature = "app")]
pub type DefaultTmuxRunner = TmuxControlRunner;
#[cfg(not(feature = "app"))]
pub type DefaultTmuxRunner = SystemCommandRunner;
#[derive(Clone, Debug)]
pub struct TmuxBackend<R = DefaultTmuxRunner> {
    program: String,
    runner: R,
    completion: Option<MuxBackendCommandCompletion>,
    #[cfg(feature = "app")]
    checked_precondition: Option<MuxScopedExecutionPrecondition>,
}

#[cfg(feature = "app")]
impl TmuxBackend<DefaultTmuxRunner> {
    pub fn new() -> Self {
        Self::with_runner("tmux", TmuxControlRunner::default())
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
            completion: None,
            #[cfg(feature = "app")]
            checked_precondition: None,
        }
    }
}

impl<R: CommandRunner> TmuxBackend<R> {
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
        #[cfg(feature = "app")]
        if let Some(precondition) = self.checked_precondition.as_ref() {
            return self.run_conditional_owned(args, precondition);
        }
        let output = self.runner.run(&self.program, &args)?;
        if output.success {
            return Ok(output.stdout);
        }
        let message = tmux_command_failure_message(&output.stderr);
        let error = if tmux_target_not_found(&message) {
            MuxBackendOperationError::Stale(message)
        } else {
            MuxBackendOperationError::Failed(message)
        };
        Err(error.into())
    }

    #[cfg(feature = "app")]
    fn run_conditional_owned(
        &self,
        args: Vec<String>,
        precondition: &MuxScopedExecutionPrecondition,
    ) -> Result<String> {
        let target = precondition
            .target
            .pane_id
            .as_deref()
            .or(precondition.target.window_id.as_deref())
            .or(precondition.target.session_id.as_deref())
            .ok_or_else(|| {
                MuxBackendOperationError::stale("tmux checked target has no identity")
            })?;
        let equality = |field: &str, expected: &str| {
            let mut predicate = String::from("#{==:#{");
            predicate.push_str(field);
            predicate.push_str("},");
            predicate.push_str(expected);
            predicate.push('}');
            predicate
        };
        let mut predicates = vec![equality(
            "session_id",
            precondition
                .target
                .session_id
                .as_deref()
                .unwrap_or_default(),
        )];
        if let Some(window_id) = precondition.target.window_id.as_deref() {
            predicates.push(equality("window_id", window_id));
        }
        if let Some(pane_id) = precondition.target.pane_id.as_deref() {
            predicates.push(equality("pane_id", pane_id));
        }
        if let Some(terminal_id) = precondition.target.terminal_id.as_deref() {
            predicates.push(equality("pane_tty", terminal_id));
        }
        if let Some(pid) = precondition
            .target
            .occupant
            .as_ref()
            .and_then(|occupant| occupant.pid)
        {
            predicates.push(equality("pane_pid", &pid.to_string()));
        }
        if let Some(server_pid) = precondition
            .occupant_fingerprint
            .as_deref()
            .and_then(tmux_server_pid_from_occupant_id)
        {
            predicates.push(equality("pid", server_pid));
        }
        let condition = predicates
            .into_iter()
            .reduce(|left, right| {
                let mut condition = String::from("#{&&:");
                condition.push_str(&left);
                condition.push(',');
                condition.push_str(&right);
                condition.push('}');
                condition
            })
            .expect("tmux checked target always has a session identity");
        let command = tmux_command_string(&args);
        let conditional_args = vec![
            "if-shell".to_owned(),
            "-F".to_owned(),
            "-t".to_owned(),
            target.to_owned(),
            condition,
            command,
            format!("display-message -p '{TMUX_STALE_TARGET_MARKER}'"),
        ];
        let output = self.runner.run(&self.program, &conditional_args)?;
        if output.success {
            if output.stdout.trim() == TMUX_STALE_TARGET_MARKER {
                return Err(MuxBackendOperationError::stale(
                    "tmux checked target changed before mutation",
                )
                .into());
            }
            return Ok(output.stdout);
        }
        let message = tmux_command_failure_message(&output.stderr);
        let error = if tmux_target_not_found(&message) {
            MuxBackendOperationError::Stale(message)
        } else {
            MuxBackendOperationError::Failed(message)
        };
        Err(error.into())
    }

    fn run_owned_allow_server_exit(&self, args: Vec<String>) -> Result<String> {
        #[cfg(feature = "app")]
        if let Some(precondition) = self.checked_precondition.as_ref() {
            return self.run_conditional_owned(args, precondition);
        }
        let output = self.runner.run(&self.program, &args)?;
        if output.success || tmux_server_exited(&output.stderr) {
            return Ok(output.stdout);
        }
        let message = tmux_command_failure_message(&output.stderr);
        let error = if tmux_target_not_found(&message) {
            MuxBackendOperationError::Stale(message)
        } else {
            MuxBackendOperationError::Failed(message)
        };
        Err(error.into())
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
            "s\x1f#{session_id}\x1f#{session_name}\x1f#{session_attached}\x1f#{session_windows}\x1f#{pane_id}\x1f#{pane_tty}\x1f#{pane_pid}\x1f#{pane_current_path}\x1f#{pane_current_command}\x1f#{pid}",
            ";",
            "list-panes",
            "-a",
            "-F",
            "p\x1f#{session_id}\x1f#{window_id}\x1f#{window_index}\x1f#{window_name}\x1f#{window_active}\x1f#{pane_active}\x1f#{pane_id}\x1f#{pane_tty}\x1f#{pane_pid}\x1f#{pane_pb_state}\x1f#{pane_pb_progress}\x1f#{pane_current_path}\x1f#{pane_current_command}\x1f#{pid}",
        ])? else {
            return Ok(MuxSnapshot::default());
        };
        let (sessions, panes) = split_tagged_snapshot(&combined);
        parse_tmux_snapshot(&sessions, &panes)
    }

    fn execute_session_launch_plan(&mut self, plan: &MuxSessionLaunchPlan) -> Result<()> {
        self.completion = None;
        plan.validate()?;
        if !supports_tmux_session_launch_plan(plan) {
            return Err(MuxBackendOperationError::unsupported(
                "tmux cannot preserve this recursive session launch plan's split ratios or shape",
            )
            .into());
        }
        let first_window = plan
            .windows
            .first()
            .expect("tmux launch support requires at least one window");
        let first_pane = first_tmux_launch_pane(&first_window.layout);
        let initial =
            self.create_tmux_launch_session(plan, first_window.name.as_deref(), first_pane)?;
        let created_session_id = initial.session_id.clone();
        let launch: Result<TmuxLaunchAllocation> = (|| {
            self.restore_tmux_launch_session_environment(
                &created_session_id,
                &plan.environment,
                first_pane,
            )?;
            self.set_tmux_launch_pane_title(&initial.target.pane_id, first_pane)?;
            let initial_pane_ids = self.materialize_tmux_launch_layout(
                &initial.target.pane_id,
                &first_window.layout,
                &plan.environment,
            )?;
            let mut windows = vec![TmuxLaunchWindow {
                window_id: initial.target.window_id.clone(),
                pane_ids: initial_pane_ids,
            }];
            for window in plan.windows.iter().skip(1) {
                let pane = first_tmux_launch_pane(&window.layout);
                let target = self.create_tmux_launch_window(
                    &created_session_id,
                    window.name.as_deref(),
                    &plan.environment,
                    pane,
                )?;
                self.set_tmux_launch_pane_title(&target.pane_id, pane)?;
                let pane_ids = self.materialize_tmux_launch_layout(
                    &target.pane_id,
                    &window.layout,
                    &plan.environment,
                )?;
                windows.push(TmuxLaunchWindow {
                    window_id: target.window_id,
                    pane_ids,
                });
            }
            let allocation = TmuxLaunchAllocation {
                session_id: created_session_id.clone(),
                windows,
            };
            validate_tmux_launch_allocation(plan, &allocation)?;
            let focused_window = allocation
                .windows
                .get(plan.focused_window)
                .expect("tmux launch support requires a focused window");
            self.run_owned(vec![
                "select-window".to_owned(),
                "-t".to_owned(),
                focused_window.window_id.clone(),
            ])?;
            Ok(allocation)
        })();

        match launch {
            Ok(allocation) => {
                let allocated = MuxAllocatedResources {
                    session_id: allocation.session_id,
                    windows: allocation
                        .windows
                        .into_iter()
                        .map(|window| MuxAllocatedWindow {
                            window_id: window.window_id,
                            pane_ids: window.pane_ids,
                        })
                        .collect(),
                };
                self.completion = Some(MuxBackendCommandCompletion {
                    target: Some(MuxEventTarget::session(allocated.session_id.clone())),
                    allocated: Some(allocated),
                });
                Ok(())
            }
            Err(error) => match self.run_owned_allow_server_exit(vec![
                "kill-session".to_owned(),
                "-t".to_owned(),
                created_session_id,
            ]) {
                Ok(_) => Err(error),
                Err(cleanup) => Err(error.context(format!(
                    "also failed to remove partial tmux session {:?}: {cleanup}",
                    plan.session_id
                ))),
            },
        }
    }

    fn create_tmux_launch_session(
        &self,
        plan: &MuxSessionLaunchPlan,
        window_name: Option<&str>,
        pane: &MuxPaneLaunch,
    ) -> Result<TmuxLaunchSessionTarget> {
        let mut args = vec![
            "new-session".to_owned(),
            "-d".to_owned(),
            "-P".to_owned(),
            "-F".to_owned(),
            format!(
                "#{{session_id}}{TMUX_FIELD_SEPARATOR}#{{window_id}}{TMUX_FIELD_SEPARATOR}#{{pane_id}}"
            ),
            "-s".to_owned(),
            plan.session_id.clone(),
        ];
        if let Some(window_name) = window_name {
            args.extend(["-n".to_owned(), window_name.to_owned()]);
        }
        append_tmux_launch_pane_options(&mut args, &plan.environment, pane);
        append_tmux_launch_pane_command(&mut args, pane);
        let output = self.run_owned(args)?;
        match parse_tmux_launch_session_target(&output) {
            Ok(target) => Ok(target),
            Err(error) => match self.run_owned_allow_server_exit(vec![
                "kill-session".to_owned(),
                "-t".to_owned(),
                plan.session_id.clone(),
            ]) {
                Ok(_) => Err(error),
                Err(cleanup) => Err(error.context(format!(
                    "also failed to remove tmux session {:?} after losing its allocated identity: {cleanup}",
                    plan.session_id
                ))),
            },
        }
    }

    /// `new-session -e` writes into the session environment, while a launch pane's values are
    /// pane-local. Restore the session baseline after its root process has started so later panes
    /// cannot inherit root-only values.
    fn restore_tmux_launch_session_environment(
        &self,
        session_id: &str,
        session_environment: &BTreeMap<String, String>,
        root_pane: &MuxPaneLaunch,
    ) -> Result<()> {
        for (name, value) in session_environment {
            if root_pane
                .environment
                .get(name)
                .is_some_and(|root_value| root_value != value)
            {
                self.run_owned(vec![
                    "set-environment".to_owned(),
                    "-t".to_owned(),
                    session_id.to_owned(),
                    name.clone(),
                    value.clone(),
                ])?;
            }
        }
        for name in root_pane.environment.keys() {
            if !session_environment.contains_key(name) {
                self.run_owned(vec![
                    "set-environment".to_owned(),
                    "-u".to_owned(),
                    "-t".to_owned(),
                    session_id.to_owned(),
                    name.clone(),
                ])?;
            }
        }
        Ok(())
    }

    fn create_tmux_launch_window(
        &self,
        session_id: &str,
        window_name: Option<&str>,
        session_environment: &BTreeMap<String, String>,
        pane: &MuxPaneLaunch,
    ) -> Result<TmuxLaunchTarget> {
        let mut args = vec![
            "new-window".to_owned(),
            "-d".to_owned(),
            "-P".to_owned(),
            "-F".to_owned(),
            format!("#{{window_id}}{TMUX_FIELD_SEPARATOR}#{{pane_id}}"),
            "-t".to_owned(),
            session_id.to_owned(),
        ];
        if let Some(window_name) = window_name {
            args.extend(["-n".to_owned(), window_name.to_owned()]);
        }
        append_tmux_launch_pane_options(&mut args, session_environment, pane);
        append_tmux_launch_pane_command(&mut args, pane);
        parse_tmux_launch_target(&self.run_owned(args)?)
    }

    fn materialize_tmux_launch_layout(
        &self,
        root_pane_id: &str,
        layout: &MuxPaneLaunchPlan,
        session_environment: &BTreeMap<String, String>,
    ) -> Result<Vec<String>> {
        match layout {
            MuxPaneLaunchPlan::Pane(_) => Ok(vec![root_pane_id.to_owned()]),
            MuxPaneLaunchPlan::Split(split) => {
                let second_pane = self.create_tmux_launch_split(
                    root_pane_id,
                    split.direction,
                    split.ratio_millis,
                    session_environment,
                    first_tmux_launch_pane(&split.second),
                )?;
                let mut pane_ids = self.materialize_tmux_launch_layout(
                    root_pane_id,
                    &split.first,
                    session_environment,
                )?;
                pane_ids.extend(self.materialize_tmux_launch_layout(
                    &second_pane,
                    &split.second,
                    session_environment,
                )?);
                Ok(pane_ids)
            }
        }
    }

    fn create_tmux_launch_split(
        &self,
        pane_id: &str,
        direction: MuxSplitDirection,
        ratio_millis: u16,
        session_environment: &BTreeMap<String, String>,
        pane: &MuxPaneLaunch,
    ) -> Result<String> {
        let flag = match direction {
            MuxSplitDirection::Right => "-h",
            MuxSplitDirection::Down => "-v",
        };
        let mut args = vec![
            "split-window".to_owned(),
            flag.to_owned(),
            "-P".to_owned(),
            "-F".to_owned(),
            "#{pane_id}".to_owned(),
            "-t".to_owned(),
            pane_id.to_owned(),
            "-p".to_owned(),
            ((1000 - ratio_millis) / 10).to_string(),
        ];
        append_tmux_launch_pane_options(&mut args, session_environment, pane);
        append_tmux_launch_pane_command(&mut args, pane);
        let pane_id = parse_tmux_launch_pane_id(&self.run_owned(args)?)?;
        self.set_tmux_launch_pane_title(&pane_id, pane)?;
        Ok(pane_id)
    }

    fn set_tmux_launch_pane_title(&self, pane_id: &str, pane: &MuxPaneLaunch) -> Result<()> {
        if let Some(title) = &pane.title {
            self.run_owned(vec![
                "select-pane".to_owned(),
                "-t".to_owned(),
                pane_id.to_owned(),
                "-T".to_owned(),
                title.clone(),
            ])?;
        }
        Ok(())
    }

    pub fn execute(&mut self, command: MuxCommand) -> Result<()> {
        self.completion = None;
        match command {
            MuxCommand::ActivateWindow {
                session_id: _,
                window_id,
            } => {
                self.run_owned(vec!["select-window".into(), "-t".into(), window_id])?;
            }
            MuxCommand::CreateSession { plan } => self.execute_session_launch_plan(&plan)?,
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
                    self.run_owned(vec![
                        "swap-window".to_owned(),
                        "-t".to_owned(),
                        target.to_owned(),
                    ])?;
                    self.run_owned(vec![
                        "select-window".to_owned(),
                        "-t".to_owned(),
                        target.to_owned(),
                    ])?;
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
                    self.run_owned(vec![
                        "swap-window".to_owned(),
                        "-t".to_owned(),
                        target.to_owned(),
                    ])?;
                    self.run_owned(vec![
                        "select-window".to_owned(),
                        "-t".to_owned(),
                        target.to_owned(),
                    ])?;
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
            MuxCommand::SelectLastPane {
                session_id,
                window_id,
            } => {
                self.run_owned(vec![
                    "last-pane".to_owned(),
                    "-t".to_owned(),
                    window_id.unwrap_or(session_id),
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
            MuxCommand::ResizePane {
                session_id,
                pane_id,
                adjustment,
            } => {
                self.run_owned(tmux_resize_args(pane_id.unwrap_or(session_id), adjustment)?)?;
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

    pub fn take_authoritative_completion(&mut self) -> Option<MuxBackendCommandCompletion> {
        self.completion.take()
    }
}

fn append_tmux_launch_pane_options(
    args: &mut Vec<String>,
    session_environment: &BTreeMap<String, String>,
    pane: &MuxPaneLaunch,
) {
    if !pane.cwd.is_empty() {
        args.extend(["-c".to_owned(), pane.cwd.clone()]);
    }
    for (name, value) in pane.effective_environment(session_environment) {
        args.extend(["-e".to_owned(), format!("{name}={value}")]);
    }
}

fn append_tmux_launch_pane_command(args: &mut Vec<String>, pane: &MuxPaneLaunch) {
    match (&pane.command, &pane.argv) {
        (Some(command), None) => args.push(command.clone()),
        (None, Some(argv)) => args.push(tmux_shell_command(argv)),
        (None, None) => {}
        (Some(_), Some(_)) => unreachable!("validated launch panes cannot have command and argv"),
    }
}

fn first_tmux_launch_pane(layout: &MuxPaneLaunchPlan) -> &MuxPaneLaunch {
    match layout {
        MuxPaneLaunchPlan::Pane(pane) => pane,
        MuxPaneLaunchPlan::Split(split) => first_tmux_launch_pane(&split.first),
    }
}

fn tmux_shell_command(argv: &[String]) -> String {
    let mut command = String::from("exec");
    for argument in argv {
        command.push(' ');
        command.push('\'');
        let mut segments = argument.split('\'');
        if let Some(first) = segments.next() {
            command.push_str(first);
        }
        for segment in segments {
            command.push_str("'\"'\"'");
            command.push_str(segment);
        }
        command.push('\'');
    }
    command
}

fn tmux_command_string(argv: &[String]) -> String {
    tmux_shell_command(argv)
        .strip_prefix("exec ")
        .unwrap_or_default()
        .to_owned()
}

fn tmux_resize_args(target: String, adjustment: MuxPaneResize) -> Result<Vec<String>> {
    if !adjustment.is_valid() {
        return Err(MuxBackendOperationError::Failed(
            "tmux pane resize requires every supplied dimension to be positive".to_owned(),
        )
        .into());
    }
    let mut args = vec!["resize-pane".to_owned()];
    match adjustment {
        MuxPaneResize::Directional { direction, cells } => {
            let flag = match direction {
                MuxDirection::Left => "-L",
                MuxDirection::Down => "-D",
                MuxDirection::Up => "-U",
                MuxDirection::Right => "-R",
            };
            args.extend([flag.to_owned(), cells.to_string()]);
        }
        MuxPaneResize::Absolute {
            columns: Some(columns),
            rows: Some(rows),
        } => args.extend([
            "-x".to_owned(),
            columns.to_string(),
            "-y".to_owned(),
            rows.to_string(),
        ]),
        MuxPaneResize::Absolute {
            columns: Some(columns),
            rows: None,
        } => args.extend(["-x".to_owned(), columns.to_string()]),
        MuxPaneResize::Absolute {
            columns: None,
            rows: Some(rows),
        } => args.extend(["-y".to_owned(), rows.to_string()]),
        MuxPaneResize::Absolute {
            columns: None,
            rows: None,
        } => unreachable!("a valid absolute resize has a supplied dimension"),
    }
    args.extend(["-t".to_owned(), target]);
    Ok(args)
}

fn parse_tmux_launch_session_target(output: &str) -> Result<TmuxLaunchSessionTarget> {
    let fields = parse_tmux_launch_fields(output, 3, "session, window, and pane")?;
    Ok(TmuxLaunchSessionTarget {
        session_id: parse_tmux_launch_id(fields[0], '$', "session")?,
        target: TmuxLaunchTarget {
            window_id: parse_tmux_launch_id(fields[1], '@', "window")?,
            pane_id: parse_tmux_launch_id(fields[2], '%', "pane")?,
        },
    })
}

fn parse_tmux_launch_target(output: &str) -> Result<TmuxLaunchTarget> {
    let fields = parse_tmux_launch_fields(output, 2, "window and pane")?;
    Ok(TmuxLaunchTarget {
        window_id: parse_tmux_launch_id(fields[0], '@', "window")?,
        pane_id: parse_tmux_launch_id(fields[1], '%', "pane")?,
    })
}

fn parse_tmux_launch_pane_id(output: &str) -> Result<String> {
    let fields = parse_tmux_launch_fields(output, 1, "pane")?;
    parse_tmux_launch_id(fields[0], '%', "pane")
}

fn parse_tmux_launch_fields<'a>(
    output: &'a str,
    expected: usize,
    identities: &str,
) -> Result<Vec<&'a str>> {
    let output = output.trim();
    if output.is_empty() || output.contains(['\r', '\n']) {
        anyhow::bail!("tmux did not report the {identities} created by launch");
    }
    let fields = output.split(TMUX_FIELD_SEPARATOR).collect::<Vec<_>>();
    if fields.len() != expected || fields.iter().any(|field| field.is_empty()) {
        anyhow::bail!("tmux did not report the {identities} created by launch");
    }
    Ok(fields)
}

fn parse_tmux_launch_id(value: &str, prefix: char, kind: &str) -> Result<String> {
    if !value.strip_prefix(prefix).is_some_and(|identity| {
        !identity.is_empty() && identity.bytes().all(|byte| byte.is_ascii_digit())
    }) {
        anyhow::bail!("tmux reported an invalid {kind} identity during launch");
    }
    Ok(value.to_owned())
}

fn validate_tmux_launch_allocation(
    plan: &MuxSessionLaunchPlan,
    allocation: &TmuxLaunchAllocation,
) -> Result<()> {
    if allocation.session_id.is_empty() {
        anyhow::bail!("tmux did not report the session allocated by launch");
    }
    if allocation.windows.len() != plan.windows.len() {
        anyhow::bail!(
            "tmux allocated {} windows for session {:?}, expected {}",
            allocation.windows.len(),
            plan.session_id,
            plan.windows.len()
        );
    }
    for (window_index, (window, expected)) in
        allocation.windows.iter().zip(&plan.windows).enumerate()
    {
        if window.window_id.is_empty()
            || allocation.windows[..window_index]
                .iter()
                .any(|previous| previous.window_id == window.window_id)
        {
            anyhow::bail!("tmux reported a non-unique window identity during launch");
        }
        let expected_panes = expected.layout.pane_count();
        if window.pane_ids.len() != expected_panes {
            anyhow::bail!(
                "tmux allocated {} panes for window {:?}, expected {}",
                window.pane_ids.len(),
                window.window_id,
                expected_panes
            );
        }
        for (pane_index, pane_id) in window.pane_ids.iter().enumerate() {
            if pane_id.is_empty()
                || window.pane_ids[..pane_index].contains(pane_id)
                || allocation.windows[..window_index]
                    .iter()
                    .any(|previous| previous.pane_ids.contains(pane_id))
            {
                anyhow::bail!("tmux reported a non-unique pane identity during launch");
            }
        }
    }
    Ok(())
}

pub(crate) fn supports_tmux_session_launch_plan(plan: &MuxSessionLaunchPlan) -> bool {
    !plan.windows.is_empty()
        && plan.focused_window < plan.windows.len()
        && plan
            .windows
            .iter()
            .all(|window| supports_tmux_launch_layout(&window.layout))
}

fn supports_tmux_launch_layout(layout: &MuxPaneLaunchPlan) -> bool {
    match layout {
        MuxPaneLaunchPlan::Pane(_) => true,
        MuxPaneLaunchPlan::Split(split) => {
            (50..=950).contains(&split.ratio_millis)
                && split.ratio_millis % 10 == 0
                && supports_tmux_launch_layout(&split.first)
                && supports_tmux_launch_layout(&split.second)
        }
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

    fn execute_checked(
        &mut self,
        scope: MuxScope,
        command: MuxCommand,
        precondition: Option<&MuxScopedExecutionPrecondition>,
    ) -> BindingOperationOutcome<Result<()>> {
        self.completion = None;
        if precondition.is_some_and(|precondition| precondition.scope != scope) {
            return BindingOperationOutcome::Supported(Err(MuxBackendOperationError::stale(
                "tmux mux binding scope changed",
            )
            .into()));
        }
        self.checked_precondition = precondition.cloned();
        let descriptor = self.capabilities(scope);
        let outcome = descriptor.invoke(
            descriptor.request(command.operation()),
            BindingOperationAvailability::Available,
            || self.execute(command),
        );
        self.checked_precondition = None;
        outcome
    }

    fn execute_session_launch(
        &mut self,
        plan: MuxSessionLaunchPlan,
    ) -> BindingOperationOutcome<Result<()>> {
        self.completion = None;
        if plan.validate().is_err() || !supports_tmux_session_launch_plan(&plan) {
            return BindingOperationOutcome::Unsupported;
        }
        BindingOperationOutcome::Supported(self.execute_session_launch_plan(&plan))
    }

    fn session_launch_capability(
        &self,
        plan: &MuxSessionLaunchPlan,
    ) -> BindingOperationOutcome<()> {
        (plan.validate().is_ok() && supports_tmux_session_launch_plan(plan))
            .then_some(())
            .map_or(
                BindingOperationOutcome::Unsupported,
                BindingOperationOutcome::Supported,
            )
    }

    fn take_authoritative_completion(&mut self) -> Option<MuxBackendCommandCompletion> {
        TmuxBackend::take_authoritative_completion(self)
    }
    fn capabilities(&self, scope: MuxScope) -> BindingCapabilityDescriptor {
        tmux_capabilities(scope)
    }

    fn event_capabilities(&self) -> Vec<MuxEventCapability> {
        self.runner.mux_event_capabilities()
    }

    fn start_event_stream(&mut self) {
        self.runner.start_mux_event_stream(&self.program);
    }

    fn drain_events(&mut self, scope: MuxScope, maximum: usize) -> Vec<MuxEvent> {
        self.runner.drain_mux_events(scope, maximum)
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
            BindingOperation::LastPane,
            BindingOperation::ResizePane,
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

fn tmux_command_failure_message(stderr: &str) -> String {
    let detail = stderr.trim();
    if detail.is_empty() {
        "command failed".to_owned()
    } else {
        detail.to_owned()
    }
}

fn tmux_target_not_found(message: &str) -> bool {
    [
        "can't find session:",
        "can't find window:",
        "can't find pane:",
    ]
    .into_iter()
    .any(|prefix| message.starts_with(prefix))
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
        let mut fields = tmux_fields(line, 7);
        let plain_underscore = line.contains('_')
            && !line.contains(TMUX_FIELD_SEPARATOR)
            && !line.contains('\t')
            && !line.contains("\\t");
        let has_terminal_field = fields.len() >= 9
            && fields
                .get(5)
                .is_some_and(|terminal| terminal.is_empty() || terminal.starts_with("/dev/"));
        if plain_underscore && !has_terminal_field {
            let with_pid = underscore_joined_tmux_fields(line, 6);
            fields = if with_pid
                .get(5)
                .is_some_and(|pid| pid.parse::<u32>().is_ok())
            {
                with_pid
            } else {
                underscore_joined_tmux_fields(line, 5)
            };
        }
        let id = fields
            .first()
            .cloned()
            .context("tmux snapshot missing session id")?;
        let name = fields
            .get(1)
            .filter(|value| !value.is_empty())
            .cloned()
            .unwrap_or_else(|| id.clone());
        let attached = fields.get(2).is_some_and(|value| value != "0");
        let pane_id = fields.get(4).filter(|value| !value.is_empty()).cloned();
        let server_pid = fields.get(9).and_then(|value| value.parse::<u32>().ok());
        let (terminal_id, pane_pid, cwd_index) = if fields.len() >= 9 {
            (
                fields.get(5).filter(|value| !value.is_empty()).cloned(),
                fields.get(6).and_then(|value| value.parse().ok()),
                7,
            )
        } else if fields.len() >= 8 {
            (None, fields.get(5).and_then(|value| value.parse().ok()), 6)
        } else {
            (None, None, 5)
        };
        let cwd = fields
            .get(cwd_index)
            .filter(|value| !value.is_empty())
            .cloned();
        let process = fields
            .get(cwd_index + 1)
            .filter(|value| !value.is_empty())
            .cloned();
        let occupant_id = tmux_occupant_id_with_server(pane_id.as_deref(), pane_pid, server_pid);
        sessions.push(MuxSession {
            id: id.clone(),
            name,
            active: attached,
            anchor: MuxPaneAnchor {
                session_id: id,
                terminal_id,
                pane_id,
                pane_pid,
                cwd,
                process,
                occupant_id,
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

/// Tmux pane IDs are stable for the lifetime of a server connection. A pane PID is the strongest
/// occupant handle, and the server PID distinguishes a fresh server that can reuse pane IDs.
fn tmux_occupant_id_with_server(
    pane_id: Option<&str>,
    pane_pid: Option<u32>,
    server_pid: Option<u32>,
) -> Option<String> {
    let pane_id = pane_id?;
    Some(match (server_pid, pane_pid) {
        (Some(server_pid), Some(pane_pid)) => {
            format!("tmux:{pane_id}:server_pid={server_pid}:pid={pane_pid}")
        }
        (Some(server_pid), None) => format!("tmux:{pane_id}:server_pid={server_pid}:lifecycle=0"),
        (None, Some(pane_pid)) => format!("tmux:{pane_id}:pid={pane_pid}"),
        (None, None) => format!("tmux:{pane_id}:lifecycle=0"),
    })
}

/// Compatibility helper for parser/unit callers that do not have a server identity.
#[cfg(test)]
fn tmux_occupant_id(pane_id: Option<&str>, pane_pid: Option<u32>) -> Option<String> {
    tmux_occupant_id_with_server(pane_id, pane_pid, None)
}
fn tmux_server_pid_from_occupant_id(identity: &str) -> Option<&str> {
    identity
        .split_once(":server_pid=")?
        .1
        .split(':')
        .next()
        .filter(|pid| !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()))
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
        let mut fields = tmux_fields(line, 11);
        if fields.len() == 1 && line.contains('_') {
            fields = underscore_joined_tmux_fields(line, 11);
            if fields.len() == 1 {
                fields = underscore_joined_tmux_fields(line, 10);
            }
            if fields.len() == 1 {
                fields = underscore_joined_tmux_fields(line, 9);
            }
        }
        if fields.len() < 9 {
            continue;
        }
        let Some(session_id) = (!fields[0].is_empty()).then(|| fields[0].clone()) else {
            continue;
        };
        let Some(window_id) = (!fields[1].is_empty()).then(|| fields[1].clone()) else {
            continue;
        };
        let Some(window_index) = fields[2].parse().ok() else {
            continue;
        };
        let window_name = fields[3].clone();
        let window_active = fields[4] != "0";
        let pane_active = fields[5] != "0";
        let pane_id = (!fields[6].is_empty()).then(|| fields[6].clone());
        let server_pid = fields.get(13).and_then(|value| value.parse::<u32>().ok());
        let (terminal_id, pane_pid, pane_pid_reported, progress, cwd_index) = if fields.len() >= 13
        {
            (
                fields.get(7).filter(|value| !value.is_empty()).cloned(),
                fields[8].parse().ok(),
                true,
                tmux_pane_progress(fields.get(9).cloned(), fields.get(10).cloned()),
                11,
            )
        } else if fields.len() >= 12 {
            (
                None,
                fields[7].parse().ok(),
                true,
                tmux_pane_progress(fields.get(8).cloned(), fields.get(9).cloned()),
                10,
            )
        } else if fields.len() >= 11 {
            (
                None,
                None,
                false,
                tmux_pane_progress(fields.get(7).cloned(), fields.get(8).cloned()),
                9,
            )
        } else {
            (None, None, false, None, 7)
        };
        let cwd = fields
            .get(cwd_index)
            .filter(|value| !value.is_empty())
            .cloned();
        let process = fields
            .get(cwd_index + 1)
            .filter(|value| !value.is_empty())
            .cloned();
        let Some(session) = sessions.iter_mut().find(|session| session.id == session_id) else {
            continue;
        };
        if window_active {
            session.active_window_id = Some(window_id.clone());
        }
        if session.anchor.pane_id.as_deref() == pane_id.as_deref() {
            if terminal_id.is_some() {
                session.anchor.terminal_id.clone_from(&terminal_id);
            }
            if pane_pid_reported {
                session.anchor.pane_pid = pane_pid;
                session.anchor.occupant_id = tmux_occupant_id_with_server(
                    session.anchor.pane_id.as_deref(),
                    pane_pid,
                    server_pid,
                );
            }
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
                    terminal_id: terminal_id.clone(),
                    pane_pid,
                    cwd: cwd.clone(),
                    process: process.clone(),
                    occupant_id: tmux_occupant_id_with_server(
                        pane_id.as_deref(),
                        pane_pid,
                        server_pid,
                    ),
                };
            }
            // A window's bar stands for every pane in it, so the busiest pane wins.
            window.progress = furthest_along(window.progress.take(), progress);
            continue;
        }
        let anchor = MuxPaneAnchor {
            occupant_id: tmux_occupant_id_with_server(pane_id.as_deref(), pane_pid, server_pid),
            session_id,
            terminal_id,
            pane_id,
            pane_pid,
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
    use std::{
        cell::RefCell,
        collections::{BTreeMap, VecDeque},
        rc::Rc,
    };

    use super::*;
    use crate::{
        command::{
            MuxPaneLaunch, MuxPaneLaunchPlan, MuxPaneResize, MuxSessionLaunchPlan, MuxSplitLaunch,
            MuxWindowLaunchPlan,
        },
        process::{CommandOutput, CommandRunner},
    };

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
    fn tmux_launch_options_inherit_session_environment_and_prefer_pane_overrides() {
        let session_environment = BTreeMap::from([
            ("INHERITED".to_owned(), "session".to_owned()),
            ("OVERRIDE".to_owned(), "session".to_owned()),
        ]);
        let pane = MuxPaneLaunch {
            cwd: "/repo".to_owned(),
            command: None,
            argv: None,
            environment: BTreeMap::from([
                ("OVERRIDE".to_owned(), "pane".to_owned()),
                ("PANE_ONLY".to_owned(), "pane".to_owned()),
            ]),
            title: None,
        };
        let mut args = Vec::new();

        append_tmux_launch_pane_options(&mut args, &session_environment, &pane);

        assert_eq!(
            args.iter().map(String::as_str).collect::<Vec<_>>(),
            vec![
                "-c",
                "/repo",
                "-e",
                "INHERITED=session",
                "-e",
                "OVERRIDE=pane",
                "-e",
                "PANE_ONLY=pane",
            ]
        );
        assert_eq!(session_environment["OVERRIDE"], "session");
        assert_eq!(pane.environment["OVERRIDE"], "pane");
    }

    #[test]
    fn tmux_restores_session_environment_after_root_pane_overrides() {
        let runner = RecordingRunner::default();
        let calls = runner.calls.clone();
        let backend = TmuxBackend::with_runner("tmux", runner);
        let session_environment = BTreeMap::from([
            ("OVERRIDDEN".to_owned(), "session".to_owned()),
            ("SAME".to_owned(), "shared".to_owned()),
            ("SESSION_ONLY".to_owned(), "shared".to_owned()),
        ]);
        let root_pane = MuxPaneLaunch {
            cwd: "/repo".to_owned(),
            command: None,
            argv: None,
            environment: BTreeMap::from([
                ("OVERRIDDEN".to_owned(), "root".to_owned()),
                ("ROOT_ONLY".to_owned(), "root".to_owned()),
                ("SAME".to_owned(), "shared".to_owned()),
            ]),
            title: None,
        };

        backend
            .restore_tmux_launch_session_environment("$1", &session_environment, &root_pane)
            .unwrap();

        assert_eq!(
            calls.borrow().as_slice(),
            [
                RecordedCall::foreground([
                    "tmux",
                    "set-environment",
                    "-t",
                    "$1",
                    "OVERRIDDEN",
                    "session",
                ]),
                RecordedCall::foreground([
                    "tmux",
                    "set-environment",
                    "-u",
                    "-t",
                    "$1",
                    "ROOT_ONLY",
                ]),
            ]
            .as_slice()
        );
    }

    #[test]
    fn tmux_session_launch_preserves_recursive_process_and_window_intent() {
        let runner = RecordingRunner {
            stdout: Rc::new(RefCell::new(VecDeque::from([
                "$1\x1f@1\x1f%1\n".to_owned(),
                String::new(),
                "%2\n".to_owned(),
                String::new(),
                "%3\n".to_owned(),
                String::new(),
                "@2\x1f%4\n".to_owned(),
                String::new(),
                String::new(),
            ]))),
            ..Default::default()
        };
        let calls = runner.calls.clone();
        let mut backend = TmuxBackend::with_runner("tmux", runner);
        let pane = |cwd: &str, argv: Option<&[&str]>, title: Option<&str>| MuxPaneLaunch {
            cwd: cwd.to_owned(),
            command: None,
            argv: argv.map(|argv| argv.iter().map(|argument| (*argument).to_owned()).collect()),
            environment: BTreeMap::from([("SESSION".to_owned(), "1".to_owned())]),
            title: title.map(str::to_owned),
        };
        let plan = MuxSessionLaunchPlan {
            session_id: "review".to_owned(),
            focus: true,
            default_cwd: "/repo".to_owned(),
            environment: BTreeMap::from([("SESSION".to_owned(), "1".to_owned())]),
            windows: vec![
                MuxWindowLaunchPlan {
                    name: Some("code".to_owned()),
                    focus: false,
                    layout: MuxPaneLaunchPlan::Split(MuxSplitLaunch {
                        direction: MuxSplitDirection::Right,
                        ratio_millis: 600,
                        first: Box::new(MuxPaneLaunchPlan::Pane(pane(
                            "/repo/docs",
                            Some(&["nvim", "README.md"]),
                            Some("docs"),
                        ))),
                        second: Box::new(MuxPaneLaunchPlan::Split(MuxSplitLaunch {
                            direction: MuxSplitDirection::Down,
                            ratio_millis: 500,
                            first: Box::new(MuxPaneLaunchPlan::Pane(pane(
                                "/repo/tests",
                                None,
                                Some("tests"),
                            ))),
                            second: Box::new(MuxPaneLaunchPlan::Pane(pane(
                                "/repo",
                                Some(&["bash", "-lc", "echo hi"]),
                                Some("shell"),
                            ))),
                        })),
                    }),
                },
                MuxWindowLaunchPlan {
                    name: Some("logs".to_owned()),
                    focus: true,
                    layout: MuxPaneLaunchPlan::Pane(pane(
                        "/repo/logs",
                        Some(&["tail", "-f", "app.log"]),
                        Some("logs"),
                    )),
                },
            ],
            focused_window: 1,
        };
        let mut nonrepresentable = plan.clone();
        let MuxPaneLaunchPlan::Split(split) = &mut nonrepresentable.windows[0].layout else {
            unreachable!("test plan starts with a split");
        };
        split.ratio_millis = 605;
        assert!(!supports_tmux_session_launch_plan(&nonrepresentable));

        backend.execute(MuxCommand::CreateSession { plan }).unwrap();

        assert_eq!(
            calls.borrow().as_slice(),
            [
                RecordedCall::foreground([
                    "tmux",
                    "new-session",
                    "-d",
                    "-P",
                    "-F",
                    "#{session_id}\x1f#{window_id}\x1f#{pane_id}",
                    "-s",
                    "review",
                    "-n",
                    "code",
                    "-c",
                    "/repo/docs",
                    "-e",
                    "SESSION=1",
                    "exec 'nvim' 'README.md'",
                ]),
                RecordedCall::foreground(["tmux", "select-pane", "-t", "%1", "-T", "docs"]),
                RecordedCall::foreground([
                    "tmux",
                    "split-window",
                    "-h",
                    "-P",
                    "-F",
                    "#{pane_id}",
                    "-t",
                    "%1",
                    "-p",
                    "40",
                    "-c",
                    "/repo/tests",
                    "-e",
                    "SESSION=1",
                ]),
                RecordedCall::foreground(["tmux", "select-pane", "-t", "%2", "-T", "tests"]),
                RecordedCall::foreground([
                    "tmux",
                    "split-window",
                    "-v",
                    "-P",
                    "-F",
                    "#{pane_id}",
                    "-t",
                    "%2",
                    "-p",
                    "50",
                    "-c",
                    "/repo",
                    "-e",
                    "SESSION=1",
                    "exec 'bash' '-lc' 'echo hi'",
                ]),
                RecordedCall::foreground(["tmux", "select-pane", "-t", "%3", "-T", "shell"]),
                RecordedCall::foreground([
                    "tmux",
                    "new-window",
                    "-d",
                    "-P",
                    "-F",
                    "#{window_id}\x1f#{pane_id}",
                    "-t",
                    "$1",
                    "-n",
                    "logs",
                    "-c",
                    "/repo/logs",
                    "-e",
                    "SESSION=1",
                    "exec 'tail' '-f' 'app.log'",
                ]),
                RecordedCall::foreground(["tmux", "select-pane", "-t", "%4", "-T", "logs"]),
                RecordedCall::foreground(["tmux", "select-window", "-t", "@2"]),
            ]
            .as_slice()
        );
    }

    #[cfg(feature = "app")]
    #[test]
    fn tmux_recursive_launch_reports_transaction_authoritative_dfs_ids() {
        let runner = RecordingRunner {
            stdout: Rc::new(RefCell::new(VecDeque::from([
                "$7\x1f@10\x1f%1\n".to_owned(),
                "%2\n".to_owned(),
                "%3\n".to_owned(),
                "@11\x1f%4\n".to_owned(),
                String::new(),
            ]))),
            ..Default::default()
        };
        let mut backend = TmuxBackend::with_runner("tmux", runner);
        let pane = |cwd: &str| MuxPaneLaunch {
            cwd: cwd.to_owned(),
            command: None,
            argv: None,
            environment: BTreeMap::new(),
            title: None,
        };
        let plan = MuxSessionLaunchPlan {
            session_id: "review".to_owned(),
            focus: true,
            default_cwd: "/repo".to_owned(),
            environment: BTreeMap::new(),
            windows: vec![
                MuxWindowLaunchPlan {
                    name: Some("code".to_owned()),
                    focus: false,
                    layout: MuxPaneLaunchPlan::Split(MuxSplitLaunch {
                        direction: MuxSplitDirection::Right,
                        ratio_millis: 600,
                        first: Box::new(MuxPaneLaunchPlan::Pane(pane("/repo/first"))),
                        second: Box::new(MuxPaneLaunchPlan::Split(MuxSplitLaunch {
                            direction: MuxSplitDirection::Down,
                            ratio_millis: 500,
                            first: Box::new(MuxPaneLaunchPlan::Pane(pane("/repo/second"))),
                            second: Box::new(MuxPaneLaunchPlan::Pane(pane("/repo/third"))),
                        })),
                    }),
                },
                MuxWindowLaunchPlan {
                    name: Some("logs".to_owned()),
                    focus: true,
                    layout: MuxPaneLaunchPlan::Pane(pane("/repo/logs")),
                },
            ],
            focused_window: 1,
        };

        backend
            .execute(MuxCommand::CreateSession { plan })
            .expect("remote daemon tmux launch must retain exact allocated IDs");
        let completion = backend
            .take_authoritative_completion()
            .expect("successful tmux launch must retain its allocated IDs");
        assert_eq!(
            completion.target,
            Some(crate::backend::MuxEventTarget::session("$7"))
        );
        assert_eq!(
            completion.allocated,
            Some(MuxAllocatedResources {
                session_id: "$7".to_owned(),
                windows: vec![
                    MuxAllocatedWindow {
                        window_id: "@10".to_owned(),
                        pane_ids: vec!["%1".to_owned(), "%2".to_owned(), "%3".to_owned()],
                    },
                    MuxAllocatedWindow {
                        window_id: "@11".to_owned(),
                        pane_ids: vec!["%4".to_owned()],
                    },
                ],
            })
        );
        assert!(
            backend.take_authoritative_completion().is_none(),
            "completion facts are consumed with one command"
        );
    }

    #[test]
    fn tmux_recursive_launch_rolls_back_when_reported_pane_ids_are_not_unique() {
        let runner = RecordingRunner {
            stdout: Rc::new(RefCell::new(VecDeque::from([
                "$1\x1f@1\x1f%1\n".to_owned(),
                "%1\n".to_owned(),
            ]))),
            ..Default::default()
        };
        let calls = runner.calls.clone();
        let mut backend = TmuxBackend::with_runner("tmux", runner);
        let pane = MuxPaneLaunch {
            cwd: "/repo".to_owned(),
            command: None,
            argv: None,
            environment: BTreeMap::new(),
            title: None,
        };
        let plan = MuxSessionLaunchPlan {
            session_id: "review".to_owned(),
            focus: false,
            default_cwd: "/repo".to_owned(),
            environment: BTreeMap::new(),
            windows: vec![MuxWindowLaunchPlan {
                name: None,
                focus: true,
                layout: MuxPaneLaunchPlan::Split(MuxSplitLaunch {
                    direction: MuxSplitDirection::Right,
                    ratio_millis: 500,
                    first: Box::new(MuxPaneLaunchPlan::Pane(pane.clone())),
                    second: Box::new(MuxPaneLaunchPlan::Pane(pane)),
                }),
            }],
            focused_window: 0,
        };

        let error = backend
            .execute(MuxCommand::CreateSession { plan })
            .expect_err("duplicate allocated pane IDs must fail the launch");

        assert!(error.to_string().contains("non-unique pane identity"));
        assert_eq!(
            calls.borrow().last(),
            Some(&RecordedCall::foreground([
                "tmux",
                "kill-session",
                "-t",
                "$1"
            ]))
        );
        assert!(
            backend.take_authoritative_completion().is_none(),
            "a rolled-back transaction must not report allocated resources"
        );
    }

    #[test]
    fn tmux_launch_identity_parser_requires_canonical_backend_ids() {
        let parsed = parse_tmux_launch_session_target("$7\x1f@10\x1f%3\n")
            .expect("tmux numeric IDs are authoritative");
        assert_eq!(parsed.session_id, "$7");
        assert_eq!(parsed.target.window_id, "@10");
        assert_eq!(parsed.target.pane_id, "%3");
        assert!(parse_tmux_launch_session_target("$session\x1f@10\x1f%3").is_err());
        assert!(parse_tmux_launch_session_target("$7\x1f@window\x1f%3").is_err());
        assert!(parse_tmux_launch_session_target("$7\x1f@10\x1f%pane").is_err());
    }

    #[test]
    fn tmux_launch_preserves_normative_commands_for_initial_new_and_split_panes() {
        let runner = RecordingRunner {
            stdout: Rc::new(RefCell::new(VecDeque::from([
                "$1\x1f@1\x1f%1\n".to_owned(),
                "%2\n".to_owned(),
                "@2\x1f%3\n".to_owned(),
            ]))),
            ..Default::default()
        };
        let calls = runner.calls.clone();
        let mut backend = TmuxBackend::with_runner("tmux", runner);
        let root_command = "printf '%s' \"$ROOT\"";
        let split_command = "printf '%s' \"$SPLIT\"";
        let new_window_command = "printf '%s' \"$NEW\"";
        let pane = |cwd: &str, command: &str| MuxPaneLaunch {
            cwd: cwd.to_owned(),
            command: Some(command.to_owned()),
            argv: None,
            environment: BTreeMap::new(),
            title: None,
        };
        let plan = MuxSessionLaunchPlan {
            session_id: "commands".to_owned(),
            focus: false,
            default_cwd: "/repo".to_owned(),
            environment: BTreeMap::new(),
            windows: vec![
                MuxWindowLaunchPlan {
                    name: None,
                    focus: false,
                    layout: MuxPaneLaunchPlan::Split(MuxSplitLaunch {
                        direction: MuxSplitDirection::Right,
                        ratio_millis: 500,
                        first: Box::new(MuxPaneLaunchPlan::Pane(pane("/repo/root", root_command))),
                        second: Box::new(MuxPaneLaunchPlan::Pane(pane(
                            "/repo/split",
                            split_command,
                        ))),
                    }),
                },
                MuxWindowLaunchPlan {
                    name: None,
                    focus: true,
                    layout: MuxPaneLaunchPlan::Pane(pane("/repo/new", new_window_command)),
                },
            ],
            focused_window: 1,
        };

        backend.execute(MuxCommand::CreateSession { plan }).unwrap();

        assert_eq!(
            calls.borrow().as_slice(),
            [
                RecordedCall::foreground([
                    "tmux",
                    "new-session",
                    "-d",
                    "-P",
                    "-F",
                    "#{session_id}\x1f#{window_id}\x1f#{pane_id}",
                    "-s",
                    "commands",
                    "-c",
                    "/repo/root",
                    root_command,
                ]),
                RecordedCall::foreground([
                    "tmux",
                    "split-window",
                    "-h",
                    "-P",
                    "-F",
                    "#{pane_id}",
                    "-t",
                    "%1",
                    "-p",
                    "50",
                    "-c",
                    "/repo/split",
                    split_command,
                ]),
                RecordedCall::foreground([
                    "tmux",
                    "new-window",
                    "-d",
                    "-P",
                    "-F",
                    "#{window_id}\x1f#{pane_id}",
                    "-t",
                    "$1",
                    "-c",
                    "/repo/new",
                    new_window_command,
                ]),
                RecordedCall::foreground(["tmux", "select-window", "-t", "@2"]),
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
                window_id: Some("@1".to_owned()),
                direction: MuxDirection::Left,
            },
            MuxCommand::SelectNextPane {
                session_id: "$1".to_owned(),
                window_id: Some("@1".to_owned()),
            },
            MuxCommand::SelectPreviousPane {
                session_id: "$1".to_owned(),
                window_id: Some("@1".to_owned()),
            },
            MuxCommand::SelectLastPane {
                session_id: "$1".to_owned(),
                window_id: Some("@1".to_owned()),
            },
            MuxCommand::ResizePane {
                session_id: "$1".to_owned(),
                pane_id: Some("%2".to_owned()),
                adjustment: MuxPaneResize::Directional {
                    direction: MuxDirection::Left,
                    cells: 4,
                },
            },
            MuxCommand::ResizePane {
                session_id: "$1".to_owned(),
                pane_id: Some("%2".to_owned()),
                adjustment: MuxPaneResize::Absolute {
                    columns: Some(120),
                    rows: Some(40),
                },
            },
            MuxCommand::SplitPane {
                session_id: "$1".to_owned(),
                pane_id: Some("%2".to_owned()),
                direction: MuxSplitDirection::Right,
            },
            MuxCommand::TogglePaneZoom {
                session_id: "$1".to_owned(),
                pane_id: Some("%2".to_owned()),
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
                RecordedCall::foreground(["tmux", "select-pane", "-t", "@1", "-L"]),
                RecordedCall::foreground(["tmux", "select-pane", "-t", "@1.+"]),
                RecordedCall::foreground(["tmux", "select-pane", "-t", "@1.-"]),
                RecordedCall::foreground(["tmux", "last-pane", "-t", "@1"]),
                RecordedCall::foreground(["tmux", "resize-pane", "-L", "4", "-t", "%2"]),
                RecordedCall::foreground([
                    "tmux",
                    "resize-pane",
                    "-x",
                    "120",
                    "-y",
                    "40",
                    "-t",
                    "%2",
                ]),
                RecordedCall::foreground(["tmux", "split-window", "-h", "-t", "%2"]),
                RecordedCall::foreground(["tmux", "resize-pane", "-Z", "-t", "%2"]),
            ]
            .as_slice()
        );
    }

    #[test]
    fn tmux_rejects_invalid_resize_before_running_a_command() {
        let runner = RecordingRunner::default();
        let calls = runner.calls.clone();
        let mut backend = TmuxBackend::with_runner("tmux", runner);

        let error = backend
            .execute(MuxCommand::ResizePane {
                session_id: "$1".to_owned(),
                pane_id: Some("%2".to_owned()),
                adjustment: MuxPaneResize::Absolute {
                    columns: Some(0),
                    rows: Some(24),
                },
            })
            .unwrap_err();

        assert!(matches!(
            error.downcast_ref::<MuxBackendOperationError>(),
            Some(MuxBackendOperationError::Failed(message))
                if message == "tmux pane resize requires every supplied dimension to be positive"
        ));
        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn tmux_select_last_pane_reports_missing_target_as_stale() {
        let runner = RecordingRunner {
            success: Rc::new(RefCell::new(VecDeque::from([false]))),
            stderr: Rc::new(RefCell::new(VecDeque::from([
                "can't find window: @2".to_owned()
            ]))),
            ..Default::default()
        };
        let mut backend = TmuxBackend::with_runner("tmux", runner);

        let error = backend
            .execute(MuxCommand::SelectLastPane {
                session_id: "$1".to_owned(),
                window_id: Some("@2".to_owned()),
            })
            .unwrap_err();

        assert_eq!(
            error.downcast_ref::<MuxBackendOperationError>(),
            Some(&MuxBackendOperationError::Stale(
                "can't find window: @2".to_owned()
            ))
        );
    }

    #[test]
    fn tmux_select_last_pane_reports_other_execution_failures_as_failed() {
        let runner = RecordingRunner {
            success: Rc::new(RefCell::new(VecDeque::from([false]))),
            stderr: Rc::new(RefCell::new(VecDeque::from([
                "permission denied".to_owned()
            ]))),
            ..Default::default()
        };
        let mut backend = TmuxBackend::with_runner("tmux", runner);

        let error = backend
            .execute(MuxCommand::SelectLastPane {
                session_id: "$1".to_owned(),
                window_id: Some("@2".to_owned()),
            })
            .unwrap_err();

        assert_eq!(
            error.downcast_ref::<MuxBackendOperationError>(),
            Some(&MuxBackendOperationError::Failed(
                "permission denied".to_owned()
            ))
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
            stdout: Rc::new(RefCell::new(VecDeque::from([[
                "s\t$1\talpha\t1\t2\t%3\t4242\t/repo\tzsh",
                "s\t$2\tbeta\t0\t1\t%4\t4243\t/tmp\tfish",
                "p\t$1\t@1\t0\teditor\t1\t1\t%3\t/repo\tnvim",
                "p\t$1\t@2\t1\tshell\t0\t1\t%5\t/repo\tzsh",
                "p\t$2\t@3\t0\tlogs\t1\t1\t%4\t/tmp\tfish",
            ]
            .join("\n")]))),
            ..Default::default()
        };
        let calls = runner.calls.clone();
        let backend = TmuxBackend::with_runner("tmux", runner);

        let snapshot = backend.snapshot().unwrap();

        assert_eq!(
            calls.borrow().len(),
            1,
            "sessions and panes should come from one tmux process"
        );
        assert_eq!(snapshot.active_session_id.as_deref(), Some("$1"));
        assert_eq!(snapshot.sessions[0].name, "alpha");
        assert_eq!(snapshot.sessions[0].anchor.pane_id.as_deref(), Some("%3"));
        assert_eq!(snapshot.sessions[0].anchor.pane_pid, Some(4242));
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
    fn tmux_snapshot_replaces_anchor_pid_from_matching_pane_row() {
        let snapshot = parse_tmux_snapshot(
            "$1\x1fboo\x1f1\x1f1\x1f%3\x1f4242\x1f/repo\x1fzsh\n",
            "$1\x1f@3\x1f0\x1fai\x1f1\x1f1\x1f%3\x1f5252\x1fnormal\x1f42\x1f/repo\x1fzsh\n",
        )
        .unwrap();

        let anchor = &snapshot.sessions[0].anchor;
        assert_eq!(anchor.pane_pid, Some(5252));
        assert_eq!(anchor.occupant_id.as_deref(), Some("tmux:%3:pid=5252"));
    }

    #[test]
    fn tmux_occupant_handle_changes_when_server_replaces_a_pane_process() {
        let first = tmux_occupant_id(Some("%7"), Some(120));
        let same_process = tmux_occupant_id(Some("%7"), Some(120));
        let replacement = tmux_occupant_id(Some("%7"), Some(121));

        assert_eq!(first, same_process);
        assert_ne!(first, replacement);
        assert_eq!(replacement.as_deref(), Some("tmux:%7:pid=121"));
    }

    #[test]
    fn tmux_checked_mutation_uses_server_side_target_conditional() {
        let runner = RecordingRunner {
            calls: Rc::default(),
            stdout: Rc::new(RefCell::new(VecDeque::from([
                TMUX_STALE_TARGET_MARKER.to_owned()
            ]))),
            stderr: Rc::default(),
            success: Rc::default(),
        };
        let calls = runner.calls.clone();
        let mut backend = TmuxBackend::with_runner("tmux", runner);
        let scope = crate::controller::MuxScope::new(
            crate::controller::SpaceId::from_persistence(7),
            crate::controller::BindingId::from_persistence(8),
        );
        let precondition = MuxScopedExecutionPrecondition {
            scope,
            target: MuxEventTarget::pane(
                "$1",
                "@1",
                "%1",
                "/dev/ttys001",
                Some(crate::backend::MuxOccupantIdentity {
                    backend_identity: "tmux:%1:pid=42".to_owned(),
                    pid: Some(42),
                    process: Some("zsh".to_owned()),
                }),
            ),
            occupant_fingerprint: Some("tmux:%1:pid=42".to_owned()),
            binding_generation: None,
            occupant_generation: None,
        };
        let outcome = backend.execute_checked(
            scope,
            MuxCommand::RenameWindow {
                session_id: "$1".to_owned(),
                window_id: "@1".to_owned(),
                name: "replacement".to_owned(),
            },
            Some(&precondition),
        );
        assert!(matches!(
            outcome,
            BindingOperationOutcome::Supported(Err(error))
                if matches!(
                    error.downcast_ref::<MuxBackendOperationError>(),
                    Some(MuxBackendOperationError::Stale(_))
                )
        ));
        let recorded = calls.borrow();
        let call = &recorded[0].argv;
        assert_eq!(call.get(1).map(String::as_str), Some("if-shell"));
        assert!(call.iter().any(|argument| argument.contains("pane_pid")));
        assert!(call.iter().any(|argument| argument.contains("%1")));
        assert!(
            call.iter()
                .any(|argument| argument.contains(TMUX_STALE_TARGET_MARKER))
        );
    }

    #[test]
    fn tmux_snapshot_recovers_underscore_joined_rows_with_pane_metadata() {
        let pane_line = "$2_@28_1_ai_1_1_%34_4242_normal_42_/Users/luan/src/agents_node";
        assert_eq!(
            underscore_joined_tmux_fields(pane_line, 10),
            vec![
                "$2".to_owned(),
                "@28".to_owned(),
                "1".to_owned(),
                "ai".to_owned(),
                "1".to_owned(),
                "1".to_owned(),
                "%34".to_owned(),
                "4242".to_owned(),
                "normal".to_owned(),
                "42".to_owned(),
                "/Users/luan/src/agents".to_owned(),
                "node".to_owned(),
            ]
        );

        let snapshot = parse_tmux_snapshot(
            "$2_agents_0_3_%34_4242_/Users/luan/src/agents_node\n",
            &format!("{pane_line}\n"),
        )
        .unwrap();

        assert_eq!(snapshot.active_session_id, None);
        assert_eq!(snapshot.sessions[0].id, "$2");
        assert_eq!(snapshot.sessions[0].name, "agents");
        assert_eq!(snapshot.sessions[0].anchor.pane_id.as_deref(), Some("%34"));
        assert_eq!(snapshot.sessions[0].anchor.pane_pid, Some(4242));
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
        assert_eq!(snapshot.sessions[0].windows[0].anchor.pane_pid, Some(4242));
        assert_eq!(
            snapshot.sessions[0].windows[0].progress,
            Some(MuxWindowProgress {
                state: "normal".to_owned(),
                percent: Some(42),
            })
        );
        assert_eq!(
            snapshot.sessions[0].windows[0].anchor.cwd.as_deref(),
            Some("/Users/luan/src/agents")
        );
        assert_eq!(
            snapshot.sessions[0].windows[0].anchor.process.as_deref(),
            Some("node")
        );
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
