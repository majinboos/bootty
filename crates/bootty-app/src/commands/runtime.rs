use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    app_actions::{AppAction, KeybindAction, MuxKeyAction},
    commands::{CommandCatalog, CommandExecutor, CoreCommandExecutor, ExactMuxTarget},
    state::{AppEffect, AppState, ViewportSnapshot},
    workspace_runtime::WorkspaceRuntime,
};
use bootty_command::{
    AppCommandReceiver, AppCommandSender, BoundAppCommandSender, Caller, CommandCancellation,
    CommandInvocation, CommandOutcome, CommandTarget, MutationClass, ResourceKind,
    app_command_channel,
};
use bootty_mux::{
    RepaintHandle,
    capability::BindingOperationOutcome,
    command::MuxCommand,
    controller::{MuxCommandCompletion, MuxCommandError, MuxCommandResult, SpaceId},
    provider::PaneTopology,
    terminal::decode_scoped_pane_id,
};
use bootty_winit::{
    input::TerminalInputCommand,
    terminal::{KeyInput, KeyMods, TerminalKey},
};
use bootty_workspace::BindingMembershipMutation;

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

fn command_target_value(target: CommandTarget) -> serde_json::Value {
    serde_json::to_value(target).expect("serialize command target")
}

const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

fn command_execution(
    execution: Option<(Instant, CommandCancellation)>,
) -> (Instant, CommandCancellation) {
    execution.unwrap_or_else(|| (Instant::now() + COMMAND_TIMEOUT, CommandCancellation::new()))
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

pub(crate) enum PendingCommandResult {
    Mux {
        scope: SpaceId,
        command: MuxCommand,
        membership: Option<Box<BindingMembershipMutation>>,
        result: mpsc::Receiver<MuxCommandResult>,
    },
    Outcome(mpsc::Receiver<CommandOutcome>),
}

pub(crate) enum CommandDispatch {
    Complete(CommandOutcome),
    Pending(PendingCommandResult),
}

pub(crate) struct PendingAppCommand {
    pub(crate) deadline: Instant,
    pub(crate) cancellation: CommandCancellation,
    pub(crate) response: Option<mpsc::Sender<CommandOutcome>>,
    pub(crate) result: PendingCommandResult,
}

pub(crate) struct CommandRuntime {
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
    pub(crate) fn new(repaint: RepaintHandle) -> Self {
        let (sender, receiver) = app_command_channel(64, repaint);
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

    pub(crate) fn queue(&mut self, invocation: CommandInvocation) {
        self.queued = Some(invocation);
    }

    pub(crate) fn clear_queue(&mut self) {
        self.queued = None;
    }

    pub(crate) fn target_kind(&self, command: &str) -> Option<ResourceKind> {
        self.catalog.describe(command)?.target
    }

    pub(crate) fn take_queued(&mut self) -> Option<CommandInvocation> {
        self.queued.take()
    }

    pub(crate) fn target_identity(&self) -> (&str, u64, u64) {
        (
            &self.instance_handle,
            self.instance_generation,
            self.window_generation,
        )
    }
}

impl AppState {
    fn reject_command(&mut self, outcome: CommandOutcome) -> CommandDispatch {
        self.last_error = command_outcome_message(&outcome);
        CommandDispatch::Complete(outcome)
    }

    /// Returns a non-blocking sender for producers outside the UI-owner call stack.
    ///
    /// UI code dispatches directly and must not synchronously wait on this channel's response.
    pub fn app_command_sender(&self, caller: Caller) -> BoundAppCommandSender {
        self.commands.sender.for_caller(caller)
    }

    pub fn command_catalog(&self) -> Arc<CommandCatalog> {
        Arc::clone(&self.commands.catalog)
    }

    pub(crate) fn drain_app_commands(
        &mut self,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) {
        self.drain_pending_app_commands(Instant::now());
        let mut drained = 0;
        for _ in 0..32 {
            let request = match self.commands.receiver.try_recv() {
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
                    self.commands.pending.push(PendingAppCommand {
                        deadline: request.deadline,
                        cancellation: request.cancellation,
                        response: Some(request.response),
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
        for pending in std::mem::take(&mut self.commands.pending) {
            let membership_scope = match &pending.result {
                PendingCommandResult::Mux {
                    scope,
                    membership: Some(_),
                    ..
                } => Some(*scope),
                _ => None,
            };
            let defer_membership = |workspace: &mut WorkspaceRuntime| {
                if let Some(scope) = membership_scope {
                    workspace.defer_binding_membership_reconciliation(scope);
                }
            };
            let outcome = if pending.cancellation.is_cancelled() {
                defer_membership(&mut self.workspace);
                CommandOutcome::cancelled()
            } else if now >= pending.deadline && pending.cancellation.cancel() {
                defer_membership(&mut self.workspace);
                CommandOutcome::deadline_exceeded()
            } else {
                match &pending.result {
                    PendingCommandResult::Mux {
                        scope,
                        command,
                        membership,
                        result,
                    } => match result.try_recv() {
                        Ok(result) => self.command_outcome_for_mux_result(
                            *scope,
                            command,
                            membership.as_deref(),
                            result,
                        ),
                        Err(mpsc::TryRecvError::Empty) => {
                            self.commands.pending.push(pending);
                            continue;
                        }
                        Err(mpsc::TryRecvError::Disconnected) => {
                            defer_membership(&mut self.workspace);
                            CommandOutcome::Failed {
                                code: "backend_worker_stopped".to_owned(),
                                message: "mux command worker stopped".to_owned(),
                            }
                        }
                    },
                    PendingCommandResult::Outcome(result) => match result.try_recv() {
                        Ok(outcome) => outcome,
                        Err(mpsc::TryRecvError::Empty) => {
                            self.commands.pending.push(pending);
                            continue;
                        }
                        Err(mpsc::TryRecvError::Disconnected) => CommandOutcome::Failed {
                            code: "command_worker_stopped".to_owned(),
                            message: "command worker stopped".to_owned(),
                        },
                    },
                }
            };
            if let Some(response) = pending.response {
                let _ = response.send(outcome);
            } else if let Some(message) = command_outcome_message(&outcome) {
                self.last_error = Some(message);
            }
        }
    }

    pub(crate) fn dispatch_command(
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
        let target_supplied = invocation.target.is_some();
        let mut resolved = match self.commands.catalog.resolve(invocation) {
            Ok(resolved) => resolved,
            Err(outcome) => return self.reject_command(outcome),
        };
        let (target, exact_target) = match self.resolve_command_target(
            &resolved.invocation.command,
            resolved.descriptor.target,
            resolved.invocation.target.as_ref(),
        ) {
            Ok(target) => target,
            Err(outcome) => return self.reject_command(outcome),
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
        let planned_mux_command = match &resolved.executor {
            CommandExecutor::Core(CoreCommandExecutor::Keybind(KeybindAction::Mux(action))) => {
                self.plan_mux_key_action(*action, exact_target.as_ref())
            }
            _ => None,
        };
        if let Some(command) = planned_mux_command.as_ref()
            && let Some(outcome) = self.preflight_mux_command(command)
        {
            return self.reject_command(outcome);
        }
        if target_supplied
            && matches!(
                &resolved.executor,
                CommandExecutor::Core(
                    CoreCommandExecutor::Keybind(KeybindAction::Write(_))
                        | CoreCommandExecutor::PasteTerminal(_)
                        | CoreCommandExecutor::SubmitTerminal
                )
            )
            && let Some(exact_target) = exact_target.as_ref()
            && let Err(outcome) = self.activate_terminal_target(exact_target)
        {
            return self.reject_command(outcome);
        }
        let caller = resolved.invocation.caller;
        match resolved.executor {
            CommandExecutor::Core(executor) => self.dispatch_resolved_command(
                executor,
                planned_mux_command,
                caller,
                viewport,
                effects,
                execution,
            ),
            CommandExecutor::Extension(handler) => {
                let (deadline, cancellation) = command_execution(execution);
                CommandDispatch::Pending(PendingCommandResult::Outcome(handler.invoke_with_target(
                    resolved.invocation,
                    deadline,
                    cancellation,
                    target_supplied,
                )))
            }
        }
    }

    fn preflight_mux_command(&self, command: &MuxCommand) -> Option<CommandOutcome> {
        if let Some(message) = self.workspace.active.binding.mux.unavailable_reason() {
            return Some(CommandOutcome::Unavailable {
                message: message.to_owned(),
            });
        }
        command_outcome_for_binding_operation(self.workspace.active.binding.mux.operation_outcome(
            &self.workspace.active.binding.multiplexer,
            command.operation(),
        ))
    }

    fn resolve_command_target(
        &self,
        command: &str,
        expected: Option<ResourceKind>,
        supplied: Option<&CommandTarget>,
    ) -> Result<(Option<CommandTarget>, Option<ExactMuxTarget>), CommandOutcome> {
        let Some(expected) = expected else {
            return if supplied.is_none() {
                Ok((None, None))
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
        if let Some(supplied) = supplied {
            if self
                .current_command_target_for(command, expected)
                .is_some_and(|current| current == *supplied)
            {
                return Ok((
                    Some(supplied.clone()),
                    self.current_exact_mux_target_for(command, expected),
                ));
            }
            if let Some(exact) = self.exact_mux_target(supplied) {
                return Ok((Some(supplied.clone()), Some(exact)));
            }
            return Err(CommandOutcome::StaleTarget {
                message: format!("the {expected:?} target is stale"),
            });
        }
        let Some(current) = self.current_command_target_for(command, expected) else {
            return Err(CommandOutcome::Unavailable {
                message: format!("no current {expected:?} target is available"),
            });
        };
        // The opaque handle is only an equality token. Build the typed target from current mux
        // state after the complete wire target (kind, handle, and generation) has matched.
        let exact = self.current_exact_mux_target_for(command, expected);
        Ok((Some(current), exact))
    }

    fn exact_mux_target(&self, target: &CommandTarget) -> Option<ExactMuxTarget> {
        let scope = self.workspace.active.binding.scope;
        let mux = &self.workspace.active.binding.mux;
        let binding = self.binding_target_handle(scope, mux.binding_generation());
        for session in mux.sessions() {
            if let Some(generation) = mux.session_generation(&session.id) {
                let session_target = CommandTarget {
                    kind: ResourceKind::Session,
                    handle: serde_json::to_string(&[&binding, &session.id])
                        .expect("serialize session target"),
                    generation,
                };
                if target == &session_target {
                    return Some(ExactMuxTarget::Session(scope, session.id.clone()));
                }
            }
            for window in &session.windows {
                if let Some(generation) = mux.window_generation(&session.id, &window.id) {
                    let window_target = CommandTarget {
                        kind: ResourceKind::MuxWindow,
                        handle: serde_json::to_string(&[&binding, &session.id, &window.id])
                            .expect("serialize window target"),
                        generation,
                    };
                    if target == &window_target {
                        return Some(ExactMuxTarget::Window(
                            scope,
                            session.id.clone(),
                            window.id.clone(),
                        ));
                    }
                }
                for pane in std::iter::once(&window.anchor).chain(&window.panes) {
                    let Some(pane_id) = pane.pane_id.as_deref() else {
                        continue;
                    };
                    let exact = ExactMuxTarget::Pane(
                        scope,
                        session.id.clone(),
                        window.id.clone(),
                        pane_id.to_owned(),
                    );
                    let Some(generation) = mux.pane_generation(&session.id, &window.id, pane_id)
                    else {
                        continue;
                    };
                    let pane_target = CommandTarget {
                        kind: ResourceKind::Pane,
                        handle: serde_json::to_string(&[
                            &binding,
                            &session.id,
                            &window.id,
                            pane_id,
                        ])
                        .expect("serialize pane target"),
                        generation,
                    };
                    if target == &pane_target {
                        return Some(exact);
                    }
                    let terminal_target = CommandTarget {
                        kind: ResourceKind::Terminal,
                        handle: pane_target.handle,
                        generation,
                    };
                    if target == &terminal_target {
                        return Some(exact);
                    }
                }
            }
        }
        None
    }

    fn activate_terminal_target(&mut self, target: &ExactMuxTarget) -> Result<(), CommandOutcome> {
        let scope = target.scope();
        let (session, window, pane) = target.ids();
        let Some(session) = session.map(str::to_owned) else {
            return Ok(());
        };
        let window = window.map(str::to_owned);
        let pane = pane.map(str::to_owned);
        self.workspace
            .activate_target(scope, &session, window.as_deref(), &self.repaint)
            .map_err(|error| CommandOutcome::Failed {
                code: "execution_failed".to_owned(),
                message: error.to_string(),
            })?;
        if let Some(pane) = pane {
            self.workspace.active.binding.focus_pane(&pane);
        }
        self.sync_native_layout_terminal_now();
        (self.repaint)();
        Ok(())
    }

    pub(crate) fn current_exact_mux_target_for(
        &self,
        command: &str,
        kind: ResourceKind,
    ) -> Option<ExactMuxTarget> {
        let scope = self.workspace.active.binding.scope;
        let (session_id, window_id, pane_id) = self.selected_mux_resource_path();
        match kind {
            ResourceKind::Binding => Some(ExactMuxTarget::Binding(scope)),
            ResourceKind::Session => session_id
                .map(|session_id| ExactMuxTarget::Session(scope, session_id))
                .or_else(|| (command == "new_tab").then_some(ExactMuxTarget::Binding(scope))),
            ResourceKind::MuxWindow => Some(ExactMuxTarget::Window(scope, session_id?, window_id?)),
            ResourceKind::Pane => Some(ExactMuxTarget::Pane(
                scope,
                session_id?,
                window_id?,
                pane_id?,
            )),
            ResourceKind::Terminal => match (session_id, window_id, pane_id) {
                (Some(session), Some(window), Some(pane)) => {
                    Some(ExactMuxTarget::Pane(scope, session, window, pane))
                }
                (Some(session), Some(window), None) => {
                    Some(ExactMuxTarget::Window(scope, session, window))
                }
                (Some(session), None, _) => Some(ExactMuxTarget::Session(scope, session)),
                (None, _, _) => Some(ExactMuxTarget::Binding(scope)),
            },
            ResourceKind::Instance | ResourceKind::ApplicationWindow => None,
        }
    }

    pub(crate) fn current_command_target_for(
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
        let binding_generation = self.workspace.active.binding.mux.binding_generation();
        let binding_handle = self.binding_target_handle(scope, binding_generation);
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

    pub(crate) fn selected_mux_resource_path(
        &self,
    ) -> (Option<String>, Option<String>, Option<String>) {
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
        planned_mux_command: Option<MuxCommand>,
        caller: Caller,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let executor = match executor {
            executor
            @ CoreCommandExecutor::Keybind(KeybindAction::App(AppAction::ReloadConfig)) => executor,
            CoreCommandExecutor::Keybind(action) => {
                return self.dispatch_resolved_keybind_command(
                    action,
                    planned_mux_command,
                    caller,
                    viewport,
                    effects,
                    execution,
                );
            }
            executor => executor,
        };
        if let Err(outcome) = Self::begin_synchronous_command(execution) {
            return CommandDispatch::Complete(outcome);
        }
        match executor {
            CoreCommandExecutor::Keybind(KeybindAction::App(AppAction::ReloadConfig)) => {
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
            CoreCommandExecutor::Sidebar(action) => {
                self.apply_sidebar_action(action);
                CommandDispatch::Complete(CommandOutcome::success())
            }
            CoreCommandExecutor::CurrentResource(kind) => {
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
                CommandDispatch::Complete(self.read_active_terminal())
            }
            CoreCommandExecutor::PasteTerminal(text) => {
                let previous_error = self.last_error.take();
                self.apply_terminal_input(TerminalInputCommand::Paste(text), effects);
                let outcome = self.last_error.clone().map_or_else(
                    || {
                        self.last_error = previous_error;
                        CommandOutcome::success()
                    },
                    |message| CommandOutcome::Failed {
                        code: "execution_failed".to_owned(),
                        message,
                    },
                );
                CommandDispatch::Complete(outcome)
            }
            CoreCommandExecutor::SubmitTerminal => {
                let previous_error = self.last_error.take();
                let key = TerminalKey::Enter;
                self.apply_terminal_input(
                    TerminalInputCommand::Key(KeyInput {
                        key,
                        mods: KeyMods::default(),
                        repeat: false,
                        utf8: None,
                        unshifted: None,
                    }),
                    effects,
                );
                let outcome = self.last_error.clone().map_or_else(
                    || {
                        self.last_error = previous_error;
                        CommandOutcome::success()
                    },
                    |message| CommandOutcome::Failed {
                        code: "execution_failed".to_owned(),
                        message,
                    },
                );
                CommandDispatch::Complete(outcome)
            }
            CoreCommandExecutor::Keybind(_) => unreachable!("keybind executors return above"),
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

    fn submit_authoritative_mux_command(
        &mut self,
        command: MuxCommand,
        membership: Option<Box<BindingMembershipMutation>>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> PendingCommandResult {
        let scope = self.workspace.active.binding.scope;
        let config = self.active_multiplexer().clone();
        let (deadline, cancellation) = command_execution(execution);
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
        PendingCommandResult::Mux {
            scope,
            command,
            membership,
            result,
        }
    }

    fn begin_authoritative_membership(
        &mut self,
        command: &MuxCommand,
    ) -> Result<Option<Box<BindingMembershipMutation>>, CommandOutcome> {
        self.workspace
            .begin_active_binding_membership_mutation(command, None)
            .map(|membership| membership.map(Box::new))
            .map_err(|error| {
                let outcome = CommandOutcome::Failed {
                    code: "persistence_failed".to_owned(),
                    message: error.to_string(),
                };
                self.last_error = command_outcome_message(&outcome);
                outcome
            })
    }

    pub(crate) fn prepare_ditch_session_command(
        &mut self,
        session_id: String,
    ) -> Result<(SpaceId, MuxCommand, Option<Box<BindingMembershipMutation>>), CommandOutcome> {
        let command = MuxCommand::DitchSession { session_id };
        let scope = self.workspace.active.binding.scope;
        if let Some(outcome) = self.preflight_mux_command(&command) {
            self.last_error = command_outcome_message(&outcome);
            return Err(outcome);
        }
        let membership = self.begin_authoritative_membership(&command)?;
        Ok((scope, command, membership))
    }

    pub(crate) fn submit_prepared_ditch_session_command(
        &mut self,
        (scope, command, membership): (SpaceId, MuxCommand, Option<Box<BindingMembershipMutation>>),
    ) {
        debug_assert_eq!(scope, self.workspace.active.binding.scope);
        let (deadline, cancellation) = command_execution(None);
        let result = self.submit_authoritative_mux_command(
            command,
            membership,
            Some((deadline, cancellation.clone())),
        );
        self.commands.pending.push(PendingAppCommand {
            deadline,
            cancellation,
            response: None,
            result,
        });
    }

    fn dispatch_resolved_keybind_command(
        &mut self,
        action: KeybindAction,
        planned_mux_command: Option<MuxCommand>,
        caller: Caller,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let mut return_native_mux_focus = false;
        if matches!(caller, Caller::Cli | Caller::Socket | Caller::Luau)
            && let KeybindAction::Mux(mux_action) = action
        {
            let process_local_action = self.workspace.active.binding.backend_policy.panes.topology
                == PaneTopology::ProcessLocal
                && Self::process_local_mux_action_uses_local_layout(mux_action);
            if process_local_action {
                return_native_mux_focus = true;
            } else if let Some(command) = planned_mux_command.clone() {
                let membership = match self.begin_authoritative_membership(&command) {
                    Ok(membership) => membership,
                    Err(outcome) => return CommandDispatch::Complete(outcome),
                };
                return CommandDispatch::Pending(
                    self.submit_authoritative_mux_command(command, membership, execution),
                );
            }
        }
        if let Err(outcome) = Self::begin_synchronous_command(execution) {
            return CommandDispatch::Complete(outcome);
        }
        let previous_error = self.last_error.take();
        self.apply_resolved_keybind_action(action, planned_mux_command, viewport, effects);
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

    fn command_outcome_for_mux_result(
        &mut self,
        scope: SpaceId,
        command: &MuxCommand,
        membership: Option<&BindingMembershipMutation>,
        result: MuxCommandResult,
    ) -> CommandOutcome {
        if let Err(error) = self
            .workspace
            .complete_binding_membership_command(scope, membership, &result)
        {
            let message = error.to_string();
            self.last_error = Some(message.clone());
            return CommandOutcome::Failed {
                code: "persistence_failed".to_owned(),
                message,
            };
        }
        let (completion, sync_error) = self.workspace.complete_authoritative_command(scope, result);
        if let Some(error) = sync_error {
            self.last_error = Some(error);
        }
        match completion {
            Ok(completion) => {
                let Some(value) = self.mux_command_completion_value(scope, command, &completion)
                else {
                    let outcome = CommandOutcome::StaleTarget {
                        message: "mux operation capability is stale".to_owned(),
                    };
                    self.last_error = command_outcome_message(&outcome);
                    return outcome;
                };
                CommandOutcome::Success {
                    value,
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
        scope: SpaceId,
        command: &MuxCommand,
        completion: &MuxCommandCompletion,
    ) -> Option<serde_json::Value> {
        let mut value = serde_json::Map::new();
        if let Some(session_id) = match command {
            MuxCommand::CreateProjectSession { session_id, .. }
            | MuxCommand::CreateWorktreeSession { session_id, .. } => Some(session_id.as_str()),
            _ => None,
        } {
            value.insert(
                "created".to_owned(),
                command_target_value(self.mux_resource_target(
                    scope,
                    ResourceKind::Session,
                    session_id,
                    None,
                )?),
            );
        }
        if let (Some(session_id), Some(window_id)) = (
            completion.selected_session.as_deref(),
            completion.selected_window.as_deref(),
        ) {
            value.insert(
                "focused".to_owned(),
                command_target_value(self.mux_resource_target(
                    scope,
                    ResourceKind::MuxWindow,
                    session_id,
                    Some(window_id),
                )?),
            );
            if matches!(command, MuxCommand::NewWindow { .. })
                && let Some(created) = self.mux_terminal_target(scope, session_id, window_id)
            {
                value.insert("created".to_owned(), command_target_value(created));
            }
        }
        if !value.contains_key("focused")
            && let Some(session_id) = completion.selected_session.as_deref()
        {
            value.insert(
                "focused".to_owned(),
                command_target_value(self.mux_resource_target(
                    scope,
                    ResourceKind::Session,
                    session_id,
                    None,
                )?),
            );
        }
        Some(serde_json::Value::Object(value))
    }

    fn mux_resource_target(
        &self,
        scope: SpaceId,
        kind: ResourceKind,
        session_id: &str,
        window_id: Option<&str>,
    ) -> Option<CommandTarget> {
        let binding_runtime = self.workspace.binding(scope)?;
        let binding = self.binding_target_handle(scope, binding_runtime.mux.binding_generation());
        let (handle, generation) = match kind {
            ResourceKind::Session => (
                serde_json::to_string(&[&binding, session_id]).expect("serialize target"),
                binding_runtime
                    .mux
                    .session_generation(session_id)
                    .unwrap_or(1),
            ),
            ResourceKind::MuxWindow => {
                let window_id = window_id.expect("mux window target requires a window id");
                (
                    serde_json::to_string(&[&binding, session_id, window_id])
                        .expect("serialize target"),
                    binding_runtime
                        .mux
                        .window_generation(session_id, window_id)
                        .unwrap_or(1),
                )
            }
            _ => unreachable!("mux completion only returns session and window targets"),
        };
        Some(CommandTarget {
            kind,
            handle,
            generation,
        })
    }

    fn mux_terminal_target(
        &self,
        scope: SpaceId,
        session_id: &str,
        window_id: &str,
    ) -> Option<CommandTarget> {
        let binding_runtime = self.workspace.binding(scope)?;
        let pane_id = binding_runtime
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id)?
            .windows
            .iter()
            .find(|window| window.id == window_id)?
            .anchor
            .pane_id
            .as_deref()?;
        let binding = self.binding_target_handle(scope, binding_runtime.mux.binding_generation());
        Some(CommandTarget {
            kind: ResourceKind::Terminal,
            handle: serde_json::to_string(&[&binding, session_id, window_id, pane_id])
                .expect("serialize terminal target"),
            generation: binding_runtime
                .mux
                .pane_generation(session_id, window_id, pane_id)?,
        })
    }

    fn binding_target_handle(&self, scope: SpaceId, generation: u64) -> String {
        let (process, _, window_generation) = self.commands.target_identity();
        serde_json::to_string(&(
            process,
            &self.window_state_key,
            window_generation,
            scope.persistence_value().to_string(),
            generation,
        ))
        .expect("serialize binding target")
    }
}
