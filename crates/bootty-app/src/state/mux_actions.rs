use bootty_mux::command::MuxCommand;

use super::{AppEffect, AppState, ViewportSnapshot};
use crate::{
    app_actions::{KeybindAction, MuxKeyAction},
    commands::ExactMuxTarget,
    workspace_runtime::mux_split_direction,
};

#[derive(Clone, Copy)]
pub(crate) enum ExactMuxAction {
    Activate,
    RelativeWindow(isize),
    LastWindow,
    NewTab,
    MoveWindow(i32),
    CloseWindowPane,
}

impl AppState {
    pub(crate) fn plan_mux_key_action(
        &mut self,
        action: MuxKeyAction,
        target: Option<&ExactMuxTarget>,
    ) -> Option<MuxCommand> {
        let exact = match action {
            MuxKeyAction::NewTab => ExactMuxAction::NewTab,
            MuxKeyAction::NextTab => ExactMuxAction::RelativeWindow(1),
            MuxKeyAction::PreviousTab => ExactMuxAction::RelativeWindow(-1),
            MuxKeyAction::LastTab => ExactMuxAction::LastWindow,
            MuxKeyAction::MoveTab(delta) => ExactMuxAction::MoveWindow(delta),
            MuxKeyAction::ClosePane => ExactMuxAction::CloseWindowPane,
            _ => return self.plan_remaining_mux_key_action(action, target),
        };
        self.plan_exact_mux_action(exact, target?)
    }

    fn plan_remaining_mux_key_action(
        &self,
        action: MuxKeyAction,
        target: Option<&ExactMuxTarget>,
    ) -> Option<MuxCommand> {
        let target =
            target.filter(|target| target.scope() == self.workspace.active.binding.scope)?;
        let (session, window_id, pane_id) = target.ids();
        let session_id = session.unwrap_or("local").to_owned();
        let window_id = window_id.map(str::to_owned);
        let pane_id = pane_id.map(str::to_owned);
        Some(match action {
            MuxKeyAction::SelectTab(index) => MuxCommand::ActivateWindowIndex { session_id, index },
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
            _ => return None,
        })
    }

    pub(crate) fn plan_exact_mux_action(
        &mut self,
        action: ExactMuxAction,
        target: &ExactMuxTarget,
    ) -> Option<MuxCommand> {
        if matches!(target, ExactMuxTarget::Binding(_)) {
            return matches!(action, ExactMuxAction::NewTab).then(|| {
                let cwd = super::default_session_cwd(self.config());
                self.workspace.project_session_command(&cwd)
            });
        }
        let binding = (target.scope() == self.workspace.active.binding.scope)
            .then_some(&mut self.workspace.active.binding)?;
        let (session_id, requested_window, requested_pane) = target.ids();
        let session_id = session_id?.to_owned();
        let requested_window = requested_window.map(str::to_owned);
        let requested_pane = requested_pane.map(str::to_owned);
        let session = binding.mux.session_by_id_or_name(&session_id)?;
        let mut windows = session.windows.iter().collect::<Vec<_>>();
        windows.sort_by_key(|window| window.index);
        let window_id = requested_window
            .or_else(|| binding.mux.selected_window().map(str::to_owned))
            .or_else(|| session.active_window_id.clone());
        let position = window_id
            .as_ref()
            .and_then(|window_id| windows.iter().position(|window| window.id == *window_id));
        match action {
            ExactMuxAction::Activate => Some(MuxCommand::ActivateWindow {
                session_id,
                window_id: window_id?,
            }),
            ExactMuxAction::RelativeWindow(delta) => {
                let next = (position? as isize + delta).rem_euclid(windows.len() as isize) as usize;
                Some(MuxCommand::ActivateWindow {
                    session_id,
                    window_id: windows[next].id.clone(),
                })
            }
            ExactMuxAction::LastWindow if windows.len() > 1 => {
                Some(MuxCommand::ActivateLastWindow { session_id })
            }
            ExactMuxAction::LastWindow => None,
            ExactMuxAction::NewTab => {
                let window = position.and_then(|position| windows.get(position).copied());
                let selected = binding
                    .mux
                    .selected_session()
                    .is_some_and(|selected| selected == session.id || selected == session.name)
                    && binding
                        .mux
                        .selected_window()
                        .map_or_else(|| session.active_window_id.as_deref(), Some)
                        == window.map(|window| window.id.as_str());
                let live = selected
                    .then(|| binding.terminal.current_working_directory().ok().flatten())
                    .flatten();
                let anchor = window
                    .and_then(|window| window.anchor.cwd.clone())
                    .or_else(|| session.anchor.cwd.clone());
                Some(MuxCommand::NewWindow {
                    session_id,
                    cwd: crate::workspace_runtime::terminal_cwd_for_mux_command(live, anchor),
                })
            }
            ExactMuxAction::MoveWindow(delta) => {
                let position = position?;
                let target_position =
                    (position as i32 + delta).clamp(0, windows.len() as i32 - 1) as usize;
                (target_position != position).then(|| {
                    let window_id = windows[position].id.clone();
                    let selected_window_id = binding.mux.selected_window().map(str::to_owned);
                    match selected_window_id {
                        Some(selected_window_id) if selected_window_id != window_id => {
                            MuxCommand::MoveWindowPreservingSelection {
                                session_id,
                                window_id,
                                delta,
                                selected_window_id,
                            }
                        }
                        _ => MuxCommand::MoveWindow {
                            session_id,
                            window_id: Some(window_id),
                            delta,
                        },
                    }
                })
            }
            ExactMuxAction::CloseWindowPane => {
                let pane_id = requested_pane.or_else(|| {
                    position.and_then(|position| windows[position].anchor.pane_id.clone())
                })?;
                Some(MuxCommand::ClosePane {
                    session_id,
                    pane_id: Some(pane_id),
                })
            }
        }
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
        let target = self.current_exact_mux_target_for_action();
        let command = self.plan_mux_key_action(action, Some(&target));
        self.apply_mux_key_action_to_target(action, command);
    }

    fn current_exact_mux_target_for_action(&self) -> ExactMuxTarget {
        let scope = self.workspace.active.binding.scope;
        match self.selected_mux_resource_path() {
            (Some(session), Some(window), Some(pane)) => {
                ExactMuxTarget::Pane(scope, session, window, pane)
            }
            (Some(session), Some(window), None) => ExactMuxTarget::Window(scope, session, window),
            (Some(session), None, _) => ExactMuxTarget::Session(scope, session),
            (None, _, _) => ExactMuxTarget::Binding(scope),
        }
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
            | MuxCommand::TogglePaneZoom { pane_id, .. } => pane_id.as_deref(),
            _ => None,
        });
        if matches!(action, MuxKeyAction::ClosePane) {
            self.close_target_pane(target_pane_id, planned_command.as_ref());
            return;
        }
        // On the native engine, killing a pane means removing the focused split leaf and
        // collapsing the layout, same as closing it.
        if self.uses_native_terminal_layout() && matches!(action, MuxKeyAction::KillPane) {
            self.close_target_pane(target_pane_id, planned_command.as_ref());
            return;
        }
        if let MuxKeyAction::SplitPane(direction) = action {
            self.workspace.active.binding.split_focused_pane(
                &self.repaint,
                direction,
                target_pane_id,
            );
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
                self.workspace.active.binding.focus_pane_relative(delta);
                return;
            }
        }

        let Some(command) = planned_command else {
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
                self.workspace
                    .active
                    .binding
                    .close_focused_pane(&self.repaint, &pane_id);
            }
            return;
        }
        let Some(command) = planned_command.cloned() else {
            return;
        };
        self.execute_mux_command(command);
        self.workspace.active.binding.terminal.discard_active_pane();
    }

    fn focus_pane_neighbor(&mut self, direction: bootty_mux::command::MuxDirection) {
        let Some(area) = self.last_pane_area else {
            return;
        };
        let gap = self.config().chrome.pane_divider_width;
        self.workspace
            .active
            .binding
            .focus_pane_neighbor(direction, area, gap);
    }

    pub(super) fn execute_mux_command(&mut self, command: MuxCommand) {
        if matches!(&command, MuxCommand::CreateProjectSession { .. }) {
            match self
                .workspace
                .create_project_session(command, &self.repaint)
            {
                Ok(true) => self.input_focus = crate::input::focus::InputFocus::Terminal,
                Ok(false) => {}
                Err(error) => self.record_error(error),
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
