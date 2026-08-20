use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use super::{
    binding_panes::mux_split_direction,
    binding_session_names::{session_cwd, suggested_session_name},
    state::{
        AppEffect, AppState, ViewportSnapshot, new_mux_session_request_with_name,
        terminal_cwd_for_mux_command,
    },
};
use crate::{
    app_actions::{AppAction, KeybindAction, MuxKeyAction},
    commands::{
        AppCommandReceiver, AppCommandRequest, AppCommandSender, BoundAppCommandSender, Caller,
        CommandCancellation, CommandCatalog, CommandExecutor, CommandInvocation, CommandOutcome,
        CommandTarget, CoreCommandExecutor, MutationClass, ResourceKind,
        app_command_channel_with_repaint,
    },
    mux::{
        RepaintHandle,
        capability::BindingOperationOutcome,
        command::MuxCommand,
        controller::{MuxCommandCompletion, MuxCommandError, MuxCommandResult},
        provider::PaneTopology,
        terminal::decode_scoped_pane_id,
    },
    workspace::BindingMembershipMutation,
};

fn command_outcome_message(outcome: &CommandOutcome) -> Option<String> {
    match outcome {
        CommandOutcome::Success { .. } => None,
        CommandOutcome::Unsupported { message }
        | CommandOutcome::Unavailable { message }
        | CommandOutcome::Denied { message }
        | CommandOutcome::StaleTarget { message }
        | CommandOutcome::Failed { message, .. } => Some(message.clone()),
        CommandOutcome::ConfirmationRequired { .. } => {
            Some("command requires confirmation".to_owned())
        }
    }
}

fn command_outcome_for_binding_operation(
    outcome: BindingOperationOutcome<()>,
) -> Option<CommandOutcome> {
    match outcome {
        BindingOperationOutcome::Supported(()) => None,
        BindingOperationOutcome::Unsupported => Some(CommandOutcome::Unsupported {
            message: "mux operation is unsupported".to_owned(),
        }),
        BindingOperationOutcome::Unavailable => Some(CommandOutcome::Unavailable {
            message: "mux operation is unavailable".to_owned(),
        }),
        BindingOperationOutcome::Stale => Some(CommandOutcome::StaleTarget {
            message: "mux operation capability is stale".to_owned(),
        }),
    }
}

static NEXT_WINDOW_GENERATION: AtomicU64 = AtomicU64::new(1);

fn process_handle() -> String {
    static HANDLE: OnceLock<String> = OnceLock::new();
    HANDLE
        .get_or_init(|| {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            format!("{}:{nanos:032x}", std::process::id())
        })
        .clone()
}

pub(super) enum PendingCommandResult {
    Mux {
        command: MuxCommand,
        membership: Option<Box<BindingMembershipMutation>>,
        result: mpsc::Receiver<MuxCommandResult>,
    },
    Outcome(mpsc::Receiver<CommandOutcome>),
}

pub(super) enum CommandDispatch {
    Complete(CommandOutcome),
    Pending(PendingCommandResult),
}

pub(super) struct PendingAppCommand {
    pub(super) deadline: Instant,
    pub(super) cancellation: CommandCancellation,
    pub(super) response: mpsc::Sender<CommandOutcome>,
    pub(super) result: PendingCommandResult,
}

pub(super) struct CommandRuntime {
    instance_handle: String,
    instance_generation: u64,
    window_generation: u64,
    queued: Option<CommandInvocation>,
    sender: AppCommandSender,
    receiver: AppCommandReceiver,
    catalog: Arc<CommandCatalog>,
    pending: Vec<PendingAppCommand>,
}

impl CommandRuntime {
    pub(super) fn new(repaint: RepaintHandle) -> Self {
        let (sender, receiver) = app_command_channel_with_repaint(64, repaint);
        Self {
            instance_handle: process_handle(),
            instance_generation: 1,
            window_generation: NEXT_WINDOW_GENERATION.fetch_add(1, Ordering::Relaxed),
            queued: None,
            sender,
            receiver,
            catalog: Arc::new(CommandCatalog::default()),
            pending: Vec::new(),
        }
    }

    pub(super) fn sender(&self, caller: Caller) -> BoundAppCommandSender {
        self.sender.for_caller(caller)
    }

    pub(super) fn catalog(&self) -> &Arc<CommandCatalog> {
        &self.catalog
    }

    pub(super) fn try_recv(&self) -> Result<AppCommandRequest, mpsc::TryRecvError> {
        self.receiver.try_recv()
    }

    pub(super) fn queue(&mut self, invocation: CommandInvocation) {
        self.queued = Some(invocation);
    }

    pub(super) fn clear_queue(&mut self) {
        self.queued = None;
    }

    pub(super) fn take_queued(&mut self) -> Option<CommandInvocation> {
        self.queued.take()
    }

    pub(super) fn push_pending(&mut self, pending: PendingAppCommand) {
        self.pending.push(pending);
    }

    pub(super) fn take_pending(&mut self) -> Vec<PendingAppCommand> {
        std::mem::take(&mut self.pending)
    }

    pub(super) fn target_identity(&self) -> (&str, u64, u64) {
        (
            &self.instance_handle,
            self.instance_generation,
            self.window_generation,
        )
    }
}

impl AppState {
    /// Returns a non-blocking sender for producers outside the UI-owner call stack.
    ///
    /// UI code dispatches directly and must not synchronously wait on this channel's response.
    pub fn app_command_sender(&self, caller: Caller) -> BoundAppCommandSender {
        self.commands.sender(caller)
    }

    pub fn command_catalog(&self) -> Arc<CommandCatalog> {
        Arc::clone(self.commands.catalog())
    }

    pub(super) fn drain_app_commands(
        &mut self,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) {
        self.drain_pending_app_commands(Instant::now());
        let mut drained = 0;
        for _ in 0..32 {
            let request = match self.commands.try_recv() {
                Ok(request) => request,
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
            };
            drained += 1;
            let now = Instant::now();
            let dispatch = if request.cancellation.is_cancelled() {
                CommandDispatch::Complete(CommandOutcome::cancelled())
            } else if now >= request.deadline {
                request.cancellation.cancel();
                CommandDispatch::Complete(CommandOutcome::deadline_exceeded())
            } else {
                self.dispatch_command_with_execution(
                    request.invocation,
                    viewport,
                    effects,
                    Some((request.deadline, request.cancellation.clone())),
                )
            };
            match dispatch {
                CommandDispatch::Complete(outcome) => {
                    let _ = request.response.send(outcome);
                }
                CommandDispatch::Pending(result) => {
                    self.commands.push_pending(PendingAppCommand {
                        deadline: request.deadline,
                        cancellation: request.cancellation,
                        response: request.response,
                        result,
                    });
                }
            }
        }
        if drained == 32 {
            effects.push(AppEffect::RequestRepaint);
        }
    }

    fn drain_pending_app_commands(&mut self, now: Instant) {
        for pending in self.commands.take_pending() {
            let outcome = if pending.cancellation.is_cancelled() {
                CommandOutcome::cancelled()
            } else if now >= pending.deadline && pending.cancellation.cancel() {
                CommandOutcome::deadline_exceeded()
            } else {
                match &pending.result {
                    PendingCommandResult::Mux {
                        command,
                        membership,
                        result,
                    } => match result.try_recv() {
                        Ok(result) => self.command_outcome_for_mux_result(
                            command,
                            membership.as_deref(),
                            result,
                        ),
                        Err(mpsc::TryRecvError::Empty) => {
                            self.commands.push_pending(pending);
                            continue;
                        }
                        Err(mpsc::TryRecvError::Disconnected) => CommandOutcome::Failed {
                            code: "backend_worker_stopped".to_owned(),
                            message: "mux command worker stopped".to_owned(),
                        },
                    },
                    PendingCommandResult::Outcome(result) => match result.try_recv() {
                        Ok(outcome) => outcome,
                        Err(mpsc::TryRecvError::Empty) => {
                            self.commands.push_pending(pending);
                            continue;
                        }
                        Err(mpsc::TryRecvError::Disconnected) => CommandOutcome::Failed {
                            code: "command_worker_stopped".to_owned(),
                            message: "command worker stopped".to_owned(),
                        },
                    },
                }
            };
            let _ = pending.response.send(outcome);
        }
    }

    pub(super) fn dispatch_command(
        &mut self,
        invocation: CommandInvocation,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) -> CommandOutcome {
        match self.dispatch_command_with_execution(invocation, viewport, effects, None) {
            CommandDispatch::Complete(outcome) => outcome,
            CommandDispatch::Pending(_) => {
                unreachable!("UI-owned command dispatch cannot return a pending backend result")
            }
        }
    }

    fn dispatch_command_with_execution(
        &mut self,
        invocation: CommandInvocation,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let mut resolved = match self.commands.catalog().resolve(invocation) {
            Ok(resolved) => resolved,
            Err(outcome) => {
                self.last_error = command_outcome_message(&outcome);
                return CommandDispatch::Complete(outcome);
            }
        };
        let target = match self.resolve_command_target(
            &resolved.invocation.command,
            resolved.descriptor.target,
            resolved.invocation.target.as_ref(),
        ) {
            Ok(target) => target,
            Err(outcome) => {
                self.last_error = command_outcome_message(&outcome);
                return CommandDispatch::Complete(outcome);
            }
        };
        resolved.invocation.target = target;
        if resolved.descriptor.mutation == MutationClass::Destructive
            && matches!(
                resolved.invocation.caller,
                Caller::Cli | Caller::Socket | Caller::Luau
            )
            && resolved.invocation.confirmation.as_ref()
                != Some(&resolved.invocation.confirmation())
        {
            return CommandDispatch::Complete(CommandOutcome::ConfirmationRequired {
                confirmation: Box::new(resolved.invocation.confirmation()),
            });
        }
        let caller = resolved.invocation.caller;
        match resolved.executor {
            CommandExecutor::Core(executor) => {
                if let Some(outcome) = self.preflight_command(&executor) {
                    self.last_error = command_outcome_message(&outcome);
                    return CommandDispatch::Complete(outcome);
                }
                self.dispatch_resolved_command(
                    executor,
                    resolved.invocation.target.as_ref(),
                    caller,
                    viewport,
                    effects,
                    execution,
                )
            }
            CommandExecutor::Extension(handler) => {
                let (deadline, cancellation) = execution.unwrap_or_else(|| {
                    (
                        Instant::now() + Duration::from_secs(10),
                        CommandCancellation::new(),
                    )
                });
                CommandDispatch::Pending(PendingCommandResult::Outcome(handler(
                    resolved.invocation,
                    deadline,
                    cancellation,
                )))
            }
        }
    }

    fn preflight_command(&self, executor: &CoreCommandExecutor) -> Option<CommandOutcome> {
        let CoreCommandExecutor::Keybind(KeybindAction::Mux(action)) = executor else {
            return None;
        };
        let operation = self.mux_operation_for_action(*action)?;
        if let Some(message) = self.workspace.active.binding.mux.unavailable_reason() {
            return Some(CommandOutcome::Unavailable {
                message: message.to_owned(),
            });
        }
        command_outcome_for_binding_operation(
            self.workspace
                .active
                .binding
                .mux
                .operation_outcome(&self.workspace.active.binding.multiplexer, operation),
        )
    }

    fn resolve_command_target(
        &self,
        command: &str,
        expected: Option<ResourceKind>,
        supplied: Option<&CommandTarget>,
    ) -> Result<Option<CommandTarget>, CommandOutcome> {
        let Some(expected) = expected else {
            return if supplied.is_none() {
                Ok(None)
            } else {
                Err(CommandOutcome::Denied {
                    message: "command does not accept a target".to_owned(),
                })
            };
        };
        if supplied.is_some_and(|target| target.kind != expected) {
            return Err(CommandOutcome::Denied {
                message: format!("command requires a {expected:?} target"),
            });
        }
        let Some(current) = self.current_command_target_for(command, expected) else {
            return Err(CommandOutcome::Unavailable {
                message: format!("no current {expected:?} target is available"),
            });
        };
        if supplied.is_some_and(|target| target != &current) {
            return Err(CommandOutcome::StaleTarget {
                message: format!("the {expected:?} target is stale"),
            });
        }
        Ok(Some(current))
    }

    pub(super) fn current_command_target_for(
        &self,
        command: &str,
        kind: ResourceKind,
    ) -> Option<CommandTarget> {
        let target = self.current_command_target(kind);
        if target.is_some() || command != "new_tab" || kind != ResourceKind::Session {
            return target;
        }
        self.current_command_target(ResourceKind::Binding)
            .map(|binding| CommandTarget {
                kind,
                handle: serde_json::to_string(&("no-session", &binding.handle))
                    .expect("serialize empty session target"),
                generation: binding.generation,
            })
    }

    fn current_command_target(&self, kind: ResourceKind) -> Option<CommandTarget> {
        let (process, instance_generation, window_generation) = self.commands.target_identity();
        let process = process.to_owned();
        let window = &self.window_state_key;
        let scope = self.workspace.active.binding.scope;
        let space = scope.space_id().persistence_value().to_string();
        let binding = scope.binding_id().persistence_value().to_string();
        let binding_generation = self.workspace.active.binding.mux.binding_generation();
        let binding_handle = serde_json::to_string(&(
            &process,
            window,
            window_generation,
            &space,
            &binding,
            binding_generation,
        ))
        .expect("serialize target");
        let (session, mux_window, pane) = self.selected_mux_resource_path();
        let target = match kind {
            ResourceKind::Instance => CommandTarget {
                kind,
                handle: process,
                generation: instance_generation,
            },
            ResourceKind::ApplicationWindow => CommandTarget {
                kind,
                handle: serde_json::to_string(&[&process, window]).expect("serialize target"),
                generation: window_generation,
            },
            ResourceKind::Binding => CommandTarget {
                kind,
                handle: binding_handle,
                generation: binding_generation,
            },
            ResourceKind::Session => {
                let session = session?;
                CommandTarget {
                    kind,
                    handle: serde_json::to_string(&[&binding_handle, &session])
                        .expect("serialize target"),
                    generation: self
                        .workspace
                        .active
                        .binding
                        .mux
                        .session_generation(&session)?,
                }
            }
            ResourceKind::MuxWindow => {
                let (session, mux_window) = (session?, mux_window?);
                CommandTarget {
                    kind,
                    handle: serde_json::to_string(&[&binding_handle, &session, &mux_window])
                        .expect("serialize target"),
                    generation: self
                        .workspace
                        .active
                        .binding
                        .mux
                        .window_generation(&session, &mux_window)?,
                }
            }
            ResourceKind::Pane => {
                let (session, mux_window, pane) = (session?, mux_window?, pane?);
                CommandTarget {
                    kind,
                    handle: serde_json::to_string(&[&binding_handle, &session, &mux_window, &pane])
                        .expect("serialize target"),
                    generation: self.workspace.active.binding.mux.pane_generation(
                        &session,
                        &mux_window,
                        &pane,
                    )?,
                }
            }
            ResourceKind::Terminal => {
                let (handle, generation) = match (session, mux_window, pane) {
                    (Some(session), Some(mux_window), Some(pane)) => (
                        serde_json::to_string(&(&binding_handle, &session, &mux_window, &pane))
                            .expect("serialize target"),
                        self.workspace.active.binding.mux.terminal_generation(
                            &session,
                            &mux_window,
                            &pane,
                        )?,
                    ),
                    (Some(session), _, _) => (
                        serde_json::to_string(&(&binding_handle, &session))
                            .expect("serialize target"),
                        self.workspace
                            .active
                            .binding
                            .mux
                            .session_generation(&session)?,
                    ),
                    (None, _, _) => (
                        serde_json::to_string(&(&binding_handle, "active_terminal"))
                            .expect("serialize target"),
                        binding_generation,
                    ),
                };
                CommandTarget {
                    kind,
                    handle,
                    generation,
                }
            }
        };
        Some(target)
    }

    fn selected_mux_resource_path(&self) -> (Option<String>, Option<String>, Option<String>) {
        let Some(anchor) = self.workspace.active.binding.mux.selected_session_anchor() else {
            return (None, None, None);
        };
        let session = anchor.session_id.clone();
        let mux_window = self
            .workspace
            .active
            .binding
            .mux
            .selected_window()
            .map(str::to_owned)
            .or_else(|| {
                self.workspace
                    .active
                    .binding
                    .mux
                    .sessions()
                    .iter()
                    .find(|candidate| candidate.id == session)
                    .and_then(|candidate| candidate.active_window_id.clone())
            });
        let pane = if self.uses_native_terminal_layout() {
            self.workspace
                .active
                .binding
                .terminal
                .focused_pane_id()
                .map(|pane_id| {
                    decode_scoped_pane_id(pane_id).map_or_else(
                        || pane_id.to_owned(),
                        |(scope, pane_id)| {
                            debug_assert_eq!(scope, self.workspace.active.binding.scope);
                            pane_id
                        },
                    )
                })
        } else {
            anchor.pane_id.clone()
        };
        (Some(session), mux_window, pane)
    }

    fn read_active_terminal(&mut self) -> CommandOutcome {
        match self.workspace.active.binding.terminal.extract_frame() {
            Ok(frame) => CommandOutcome::Success {
                value: serde_json::json!({
                    "cols": frame.cols,
                    "rows": frame.rows,
                    "text": frame.text_rows().join("\n"),
                    "cursor": frame.cursor.map(|cursor| serde_json::json!({
                        "x": cursor.x,
                        "y": cursor.y,
                    })),
                }),
                warnings: Vec::new(),
            },
            Err(error) => CommandOutcome::Failed {
                code: "terminal_read_failed".to_owned(),
                message: error.to_string(),
            },
        }
    }

    fn dispatch_resolved_command(
        &mut self,
        executor: CoreCommandExecutor,
        target: Option<&CommandTarget>,
        caller: Caller,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        match executor {
            CoreCommandExecutor::Keybind(KeybindAction::App(AppAction::ReloadConfig)) => {
                if let Err(outcome) = Self::begin_synchronous_command(execution) {
                    return CommandDispatch::Complete(outcome);
                }
                let reloaded = self.reload_config(effects);
                let outcome = if reloaded {
                    self.last_error
                        .clone()
                        .map_or_else(CommandOutcome::success, |warning| {
                            CommandOutcome::success_with_warning("configuration_warning", warning)
                        })
                } else {
                    CommandOutcome::Failed {
                        code: "execution_failed".to_owned(),
                        message: self
                            .last_error
                            .clone()
                            .unwrap_or_else(|| "configuration reload failed".to_owned()),
                    }
                };
                CommandDispatch::Complete(outcome)
            }
            CoreCommandExecutor::Keybind(action) => self.dispatch_resolved_keybind_command(
                action, target, caller, viewport, effects, execution,
            ),
            CoreCommandExecutor::Sidebar(action) => {
                if let Err(outcome) = Self::begin_synchronous_command(execution) {
                    return CommandDispatch::Complete(outcome);
                }
                self.apply_sidebar_action(action);
                CommandDispatch::Complete(CommandOutcome::success())
            }
            CoreCommandExecutor::CurrentResource(kind) => {
                if let Err(outcome) = Self::begin_synchronous_command(execution) {
                    return CommandDispatch::Complete(outcome);
                }
                let outcome = self.current_command_target(kind).map_or_else(
                    || CommandOutcome::Unavailable {
                        message: format!("no current {kind:?} target is available"),
                    },
                    |target| CommandOutcome::Success {
                        value: serde_json::json!({"target": target}),
                        warnings: Vec::new(),
                    },
                );
                CommandDispatch::Complete(outcome)
            }
            CoreCommandExecutor::ReadTerminal => {
                if let Err(outcome) = Self::begin_synchronous_command(execution) {
                    return CommandDispatch::Complete(outcome);
                }
                CommandDispatch::Complete(self.read_active_terminal())
            }
        }
    }

    fn begin_synchronous_command(
        execution: Option<(Instant, CommandCancellation)>,
    ) -> Result<(), CommandOutcome> {
        let Some((deadline, cancellation)) = execution else {
            return Ok(());
        };
        if Instant::now() >= deadline && cancellation.cancel() {
            return Err(CommandOutcome::deadline_exceeded());
        }
        if !cancellation.try_start() {
            return Err(CommandOutcome::cancelled());
        }
        Ok(())
    }

    fn dispatch_resolved_keybind_command(
        &mut self,
        action: KeybindAction,
        target: Option<&CommandTarget>,
        caller: Caller,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let mut return_native_mux_focus = false;
        if matches!(caller, Caller::Cli | Caller::Socket | Caller::Luau)
            && let KeybindAction::Mux(mux_action) = action
        {
            if let Some(operation) = self.mux_operation_for_action(mux_action)
                && let Some(outcome) = command_outcome_for_binding_operation(
                    self.workspace
                        .active
                        .binding
                        .mux
                        .operation_outcome(&self.workspace.active.binding.multiplexer, operation),
                )
            {
                self.last_error = command_outcome_message(&outcome);
                return CommandDispatch::Complete(outcome);
            }
            let process_local_action = self.workspace.active.binding.backend_policy.panes.topology
                == PaneTopology::ProcessLocal
                && Self::process_local_mux_action_uses_local_layout(mux_action);
            if process_local_action {
                return_native_mux_focus = true;
            } else if let Some(command) = self.mux_command_for_command(mux_action, target) {
                let membership = match self
                    .workspace
                    .begin_active_binding_membership_mutation(&command, None)
                {
                    Ok(membership) => membership,
                    Err(error) => {
                        let outcome = CommandOutcome::Failed {
                            code: "persistence_failed".to_owned(),
                            message: error.to_string(),
                        };
                        self.last_error = command_outcome_message(&outcome);
                        return CommandDispatch::Complete(outcome);
                    }
                };
                let config = self.active_multiplexer().clone();
                let (deadline, cancellation) = execution.unwrap_or_else(|| {
                    (
                        Instant::now() + Duration::from_secs(10),
                        CommandCancellation::new(),
                    )
                });
                let result = self
                    .workspace
                    .active
                    .binding
                    .mux
                    .execute_command_authoritatively(
                        &self.repaint,
                        &config,
                        command.clone(),
                        deadline,
                        cancellation,
                    );
                return CommandDispatch::Pending(PendingCommandResult::Mux {
                    command,
                    membership: membership.map(Box::new),
                    result,
                });
            }
        }
        if let Err(outcome) = Self::begin_synchronous_command(execution) {
            return CommandDispatch::Complete(outcome);
        }
        let previous_error = self.last_error.take();
        self.apply_resolved_keybind_action(action, target, viewport, effects);
        let outcome = match self.last_error.clone() {
            Some(message) => CommandOutcome::Failed {
                code: "execution_failed".to_owned(),
                message,
            },
            None => {
                self.last_error = previous_error;
                if return_native_mux_focus {
                    CommandOutcome::Success {
                        value: self.current_mux_focus_value(),
                        warnings: Vec::new(),
                    }
                } else {
                    CommandOutcome::success()
                }
            }
        };
        CommandDispatch::Complete(outcome)
    }
    fn process_local_mux_action_uses_local_layout(action: MuxKeyAction) -> bool {
        matches!(
            action,
            MuxKeyAction::NextSession
                | MuxKeyAction::PreviousSession
                | MuxKeyAction::LastSession
                | MuxKeyAction::SelectSession(_)
                | MuxKeyAction::MoveSession(_)
                | MuxKeyAction::SplitPane(_)
                | MuxKeyAction::SelectPane(_)
                | MuxKeyAction::NextPane
                | MuxKeyAction::PreviousPane
                | MuxKeyAction::KillPane
                | MuxKeyAction::ClosePane
        )
    }

    fn current_mux_focus_value(&self) -> serde_json::Value {
        let focused = self
            .current_command_target(ResourceKind::Pane)
            .or_else(|| self.current_command_target(ResourceKind::MuxWindow))
            .or_else(|| self.current_command_target(ResourceKind::Session));
        focused.map_or_else(
            || serde_json::json!({}),
            |focused| serde_json::json!({ "focused": focused }),
        )
    }

    fn mux_command_for_command(
        &mut self,
        action: MuxKeyAction,
        target: Option<&CommandTarget>,
    ) -> Option<MuxCommand> {
        if matches!(
            action,
            MuxKeyAction::NextSession
                | MuxKeyAction::PreviousSession
                | MuxKeyAction::LastSession
                | MuxKeyAction::SelectSession(_)
                | MuxKeyAction::MoveSession(_)
        ) {
            return None;
        }

        let target = target.expect("mux command target was resolved");
        let path = serde_json::from_str::<Vec<String>>(&target.handle)
            .expect("resolved mux command target has a resource path");
        if action == MuxKeyAction::NewTab && path.first().is_some_and(|part| part == "no-session") {
            let remote = self.active_multiplexer().remote.is_some();
            let cwd = new_mux_session_request_with_name(self.config(), "").cwd;
            let cwd = session_cwd(&cwd, remote);
            let display_name = suggested_session_name(&cwd, remote);
            let session_id = crate::strings::unique_session_name(
                &display_name,
                self.taken_session_names(None).iter().map(String::as_str),
            );
            return Some(MuxCommand::CreateProjectSession { session_id, cwd });
        }

        let session_id = path
            .get(1)
            .expect("resolved mux target includes a session")
            .clone();
        let window_id = (target.kind == ResourceKind::MuxWindow).then(|| {
            path.get(2)
                .expect("mux window target includes a window")
                .clone()
        });
        let pane_id = (target.kind == ResourceKind::Pane)
            .then(|| path.get(3).expect("pane target includes a pane").clone());
        let cwd = terminal_cwd_for_mux_command(
            self.workspace
                .active
                .binding
                .terminal
                .current_working_directory()
                .ok()
                .flatten(),
            self.workspace
                .active
                .binding
                .mux
                .selected_session_anchor()
                .and_then(|anchor| anchor.cwd.clone()),
        );
        let command = match action {
            MuxKeyAction::NewTab => MuxCommand::NewWindow { session_id, cwd },
            MuxKeyAction::NextTab => MuxCommand::ActivateNextWindow { session_id },
            MuxKeyAction::PreviousTab => MuxCommand::ActivatePreviousWindow { session_id },
            MuxKeyAction::LastTab => MuxCommand::ActivateLastWindow { session_id },
            MuxKeyAction::SelectTab(index) => MuxCommand::ActivateWindowIndex { session_id, index },
            MuxKeyAction::MoveTab(delta) => MuxCommand::MoveWindow {
                session_id,
                window_id: self
                    .workspace
                    .active
                    .binding
                    .mux
                    .selected_window()
                    .map(str::to_owned),
                delta,
            },
            MuxKeyAction::SplitPane(direction) => MuxCommand::SplitPane {
                session_id,
                pane_id,
                direction: mux_split_direction(direction),
            },
            MuxKeyAction::SelectPane(direction) => MuxCommand::SelectPane {
                session_id,
                window_id,
                direction,
            },
            MuxKeyAction::NextPane => MuxCommand::SelectNextPane {
                session_id,
                window_id,
            },
            MuxKeyAction::PreviousPane => MuxCommand::SelectPreviousPane {
                session_id,
                window_id,
            },
            MuxKeyAction::KillPane => MuxCommand::KillPane {
                session_id,
                pane_id,
            },
            MuxKeyAction::ClosePane => MuxCommand::ClosePane {
                session_id,
                pane_id,
            },
            MuxKeyAction::TogglePaneZoom => MuxCommand::TogglePaneZoom {
                session_id,
                pane_id,
            },
            MuxKeyAction::NextSession
            | MuxKeyAction::PreviousSession
            | MuxKeyAction::LastSession
            | MuxKeyAction::SelectSession(_)
            | MuxKeyAction::MoveSession(_) => unreachable!("handled before command construction"),
        };
        Some(command)
    }

    fn command_outcome_for_mux_result(
        &mut self,
        command: &MuxCommand,
        membership: Option<&BindingMembershipMutation>,
        result: MuxCommandResult,
    ) -> CommandOutcome {
        let config = self.active_multiplexer().clone();
        let membership_committable = result
            .as_ref()
            .is_ok_and(|completion| completion.matches_config(&config));
        if membership.is_some() && !membership_committable {
            self.workspace
                .defer_active_binding_membership_reconciliation();
        }
        if let Ok(completion) = &result
            && completion.matches_config(&config)
            && let Some(membership) = membership
            && let Err(error) = self
                .workspace
                .commit_active_binding_membership_mutation(membership)
        {
            let message = error.to_string();
            self.workspace
                .defer_active_binding_membership_reconciliation();
            self.last_error = Some(message.clone());
            return CommandOutcome::Failed {
                code: "persistence_failed".to_owned(),
                message,
            };
        }
        match self
            .workspace
            .active
            .binding
            .mux
            .complete_authoritative_command(result, &config)
        {
            Ok(completion) => {
                self.sync_native_layout_terminal_now();
                CommandOutcome::Success {
                    value: self.mux_command_completion_value(command, &completion),
                    warnings: Vec::new(),
                }
            }
            Err(error) => {
                let message = error.to_string();
                let outcome = match error {
                    MuxCommandError::Cancelled => CommandOutcome::Failed {
                        code: "cancelled".to_owned(),
                        message,
                    },
                    MuxCommandError::DeadlineExceeded => CommandOutcome::Failed {
                        code: "deadline_exceeded".to_owned(),
                        message,
                    },
                    MuxCommandError::Unsupported => CommandOutcome::Unsupported { message },
                    MuxCommandError::Unavailable => CommandOutcome::Unavailable { message },
                    MuxCommandError::Stale => CommandOutcome::StaleTarget { message },
                    MuxCommandError::Failed(_) => CommandOutcome::Failed {
                        code: "execution_failed".to_owned(),
                        message,
                    },
                };
                self.last_error = command_outcome_message(&outcome);
                outcome
            }
        }
    }

    fn mux_command_completion_value(
        &self,
        command: &MuxCommand,
        completion: &MuxCommandCompletion,
    ) -> serde_json::Value {
        let mut value = serde_json::Map::new();
        if let Some(session_id) = match command {
            MuxCommand::CreateProjectSession { session_id, .. }
            | MuxCommand::CreateWorktreeSession { session_id, .. } => Some(session_id.as_str()),
            _ => None,
        } {
            value.insert(
                "created".to_owned(),
                serde_json::to_value(self.mux_resource_target(
                    ResourceKind::Session,
                    session_id,
                    None,
                ))
                .expect("serialize command target"),
            );
        }
        if let (Some(session_id), Some(window_id)) = (
            completion.selected_session.as_deref(),
            completion.selected_window.as_deref(),
        ) {
            value.insert(
                "focused".to_owned(),
                serde_json::to_value(self.mux_resource_target(
                    ResourceKind::MuxWindow,
                    session_id,
                    Some(window_id),
                ))
                .expect("serialize command target"),
            );
        }
        if !value.contains_key("focused")
            && let Some(session_id) = completion.selected_session.as_deref()
        {
            value.insert(
                "focused".to_owned(),
                serde_json::to_value(self.mux_resource_target(
                    ResourceKind::Session,
                    session_id,
                    None,
                ))
                .expect("serialize command target"),
            );
        }
        serde_json::Value::Object(value)
    }

    fn mux_resource_target(
        &self,
        kind: ResourceKind,
        session_id: &str,
        window_id: Option<&str>,
    ) -> CommandTarget {
        let binding = self
            .current_command_target(ResourceKind::Binding)
            .expect("mux completion requires a binding target")
            .handle;
        match kind {
            ResourceKind::Session => CommandTarget {
                kind,
                handle: serde_json::to_string(&[&binding, session_id]).expect("serialize target"),
                generation: self
                    .workspace
                    .active
                    .binding
                    .mux
                    .session_generation(session_id)
                    .unwrap_or(1),
            },
            ResourceKind::MuxWindow => {
                let window_id = window_id.expect("mux window target requires a window id");
                CommandTarget {
                    kind,
                    handle: serde_json::to_string(&[&binding, session_id, window_id])
                        .expect("serialize target"),
                    generation: self
                        .workspace
                        .active
                        .binding
                        .mux
                        .window_generation(session_id, window_id)
                        .unwrap_or(1),
                }
            }
            _ => unreachable!("mux completion only returns session and window targets"),
        }
    }
}
