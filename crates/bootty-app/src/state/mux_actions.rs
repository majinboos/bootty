use bootty_command::{CommandOutcome, CommandTarget, ResourceKind};

use super::{AppEffect, AppState, ViewportSnapshot};
use crate::{
    app_actions::{KeybindAction, MuxKeyAction},
    mux::command::MuxCommand,
    workspace_runtime::mux_split_direction,
};

fn malformed_target(message: impl Into<String>) -> CommandOutcome {
    CommandOutcome::Denied {
        message: message.into(),
    }
}

fn target_path(target: &CommandTarget) -> Result<Vec<String>, CommandOutcome> {
    serde_json::from_str(&target.handle)
        .map_err(|_| malformed_target("mux command target is malformed"))
}

impl AppState {
    /// Build the one backend command for a mux action. `None` means that Bootty owns the action.
    pub(crate) fn mux_command_for_action(
        &mut self,
        action: MuxKeyAction,
        target: Option<&CommandTarget>,
    ) -> Result<Option<MuxCommand>, CommandOutcome> {
        if matches!(
            action,
            MuxKeyAction::NextSession
                | MuxKeyAction::PreviousSession
                | MuxKeyAction::LastSession
                | MuxKeyAction::SelectSession(_)
                | MuxKeyAction::MoveSession(_)
        ) {
            return Ok(None);
        }

        let path = target.map(target_path).transpose()?;
        let selected_session = self.workspace.active.binding.mux.selected_session();
        let creates_project_session = action == MuxKeyAction::NewTab
            && (selected_session.is_none()
                || path
                    .as_ref()
                    .is_some_and(|path| path.first().is_some_and(|part| part == "no-session")));
        if creates_project_session {
            let cwd = super::new_mux_session_request_with_name(self.config(), "").cwd;
            return Ok(Some(self.workspace.project_session_command(&cwd)));
        }

        let session_id = path
            .as_ref()
            .and_then(|path| path.get(1))
            .cloned()
            .or_else(|| selected_session.map(str::to_owned))
            .unwrap_or_else(|| "local".to_owned());
        let window_id = match target.map(|target| target.kind) {
            Some(ResourceKind::MuxWindow | ResourceKind::Pane) => Some(
                path.as_ref()
                    .and_then(|path| path.get(2))
                    .cloned()
                    .ok_or_else(|| malformed_target("mux command target has no window"))?,
            ),
            _ => None,
        };
        let pane_id = match target.map(|target| target.kind) {
            Some(ResourceKind::Pane) => Some(
                path.as_ref()
                    .and_then(|path| path.get(3))
                    .cloned()
                    .ok_or_else(|| malformed_target("mux command target has no pane"))?,
            ),
            _ => None,
        };
        let cwd = super::terminal_cwd_for_mux_command(
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
        Ok(Some(command))
    }

    pub(crate) fn apply_resolved_keybind_action(
        &mut self,
        action: KeybindAction,
        command: Option<MuxCommand>,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) {
        if let KeybindAction::Mux(action) = action {
            self.apply_mux_key_action_to_target(action, command);
            effects.push(AppEffect::RequestRepaint);
        } else {
            self.apply_keybind_action(action, viewport, effects);
        }
    }

    pub(super) fn apply_mux_key_action(&mut self, action: MuxKeyAction) {
        let command = self.mux_command_for_action(action, None).ok().flatten();
        self.apply_mux_key_action_to_target(action, command);
    }

    fn apply_mux_key_action_to_target(
        &mut self,
        action: MuxKeyAction,
        planned_command: Option<MuxCommand>,
    ) {
        if self.apply_session_navigation_action(action) {
            return;
        }
        if let MuxKeyAction::MoveSession(delta) = action {
            self.move_selected_session(delta);
            return;
        }

        let target_pane_id = planned_command.as_ref().and_then(|command| match command {
            MuxCommand::SplitPane { pane_id, .. }
            | MuxCommand::KillPane { pane_id, .. }
            | MuxCommand::ClosePane { pane_id, .. }
            | MuxCommand::TogglePaneZoom { pane_id, .. } => pane_id.clone(),
            _ => None,
        });
        if matches!(action, MuxKeyAction::ClosePane) {
            self.close_target_pane(target_pane_id.as_deref(), planned_command.as_ref());
            return;
        }
        // On the native engine, killing a pane means removing the focused split leaf and
        // collapsing the layout, same as closing it.
        if self.uses_native_terminal_layout() && matches!(action, MuxKeyAction::KillPane) {
            self.close_target_pane(target_pane_id.as_deref(), planned_command.as_ref());
            return;
        }
        if let MuxKeyAction::SplitPane(direction) = action {
            self.split_focused_pane(direction, target_pane_id.as_deref());
            return;
        }
        // Native directional pane selection belongs to the local geometry.
        if let MuxKeyAction::SelectPane(direction) = action
            && self.uses_native_terminal_layout()
        {
            self.focus_pane_neighbor(direction);
            return;
        }
        if self.uses_native_terminal_layout() {
            let delta = match action {
                MuxKeyAction::NextPane => Some(1),
                MuxKeyAction::PreviousPane => Some(-1),
                _ => None,
            };
            if let Some(delta) = delta {
                self.focus_pane_relative(delta);
                return;
            }
        }

        let Some(command) =
            planned_command.or_else(|| self.mux_command_for_action(action, None).ok().flatten())
        else {
            return;
        };
        self.execute_mux_command(command);
    }

    fn close_target_pane(
        &mut self,
        target_pane_id: Option<&str>,
        planned_command: Option<&MuxCommand>,
    ) {
        if self.uses_native_terminal_layout() {
            if let Some(pane_id) = target_pane_id
                .map(str::to_owned)
                .or_else(|| self.focused_pane())
            {
                self.close_pane(&pane_id);
            }
            return;
        }
        let Some(command) = planned_command.cloned().or_else(|| {
            self.mux_command_for_action(MuxKeyAction::ClosePane, None)
                .ok()
                .flatten()
        }) else {
            return;
        };
        self.execute_mux_command(command);
        self.workspace.active.binding.terminal.discard_active_pane();
    }

    fn split_focused_pane(
        &mut self,
        direction: crate::layout::SplitDirection,
        target_pane_id: Option<&str>,
    ) {
        self.workspace
            .active
            .binding
            .split_focused_pane(&self.repaint, direction, target_pane_id);
    }

    fn focus_pane_neighbor(&mut self, direction: crate::mux::command::MuxDirection) {
        let Some(area) = self.last_pane_area else {
            return;
        };
        let gap = self.config().chrome.pane_divider_width;
        self.workspace
            .active
            .binding
            .focus_pane_neighbor(direction, area, gap);
    }

    fn focus_pane_relative(&mut self, delta: isize) {
        self.workspace.active.binding.focus_pane_relative(delta);
    }

    /// Close a specific native pane and reactivate the surviving focused pane this frame.
    fn close_pane(&mut self, pane_id: &str) {
        self.workspace
            .active
            .binding
            .close_focused_pane(&self.repaint, pane_id);
    }

    pub(super) fn execute_mux_command(&mut self, command: MuxCommand) {
        if matches!(&command, MuxCommand::CreateProjectSession { .. }) {
            match self
                .workspace
                .create_project_session(command, &self.repaint)
            {
                Ok(true) => self.input_focus = crate::input::focus::InputFocus::Terminal,
                Ok(false) => {}
                Err(error) => self.last_error = Some(error.to_string()),
            }
            self.sync_native_layout_terminal_now();
            return;
        }
        let mux_config = self.active_multiplexer().clone();
        self.workspace
            .active
            .binding
            .mux
            .execute_command(&self.repaint, &mux_config, command);
        self.sync_native_layout_terminal_now();
    }
}
