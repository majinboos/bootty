use super::{state::terminal_cwd_for_mux_command, workspace_runtime::BindingRuntime};

use crate::mux::{RepaintHandle, command::MuxCommand};

impl BindingRuntime {
    pub(super) fn relative_session_id(&self, session_id: &str, delta: isize) -> Option<String> {
        let sessions = self.mux.sessions();
        let current = sessions
            .iter()
            .position(|session| session.id == session_id || session.name == session_id)?;
        let next = (current as isize + delta).rem_euclid(sessions.len() as isize) as usize;
        Some(sessions[next].id.clone())
    }

    pub(super) fn previous_session_id(&self) -> Option<String> {
        self.mux.previous_selected_session().map(str::to_owned)
    }

    pub(super) fn relative_window_target(
        &self,
        session_id: &str,
        window_id: &str,
        delta: isize,
    ) -> Option<(String, String)> {
        self.mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .and_then(|session| {
                let mut windows = session.windows.iter().collect::<Vec<_>>();
                windows.sort_by_key(|window| window.index);
                let current = windows.iter().position(|window| window.id == window_id)?;
                let next = (current as isize + delta).rem_euclid(windows.len() as isize) as usize;
                Some((session.id.clone(), windows[next].id.clone()))
            })
    }

    pub(super) fn activate_last_window(
        &mut self,
        repaint: &RepaintHandle,
        session_id: &str,
    ) -> bool {
        let Some(session_id) = self
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .filter(|session| session.windows.len() > 1)
            .map(|session| session.id.clone())
        else {
            return false;
        };
        let config = self.multiplexer.clone();
        self.mux.execute_command(
            repaint,
            &config,
            MuxCommand::ActivateLastWindow { session_id },
        );
        true
    }

    pub(super) fn new_tab_for_window(
        &mut self,
        repaint: &RepaintHandle,
        session_id: &str,
        window_id: &str,
    ) -> bool {
        let selected_session = self.mux.selected_session().map(str::to_owned);
        let selected_window = self.mux.selected_window().map(str::to_owned);
        let Some((session_id, anchor_cwd, target_is_current)) = self
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .and_then(|session| {
                let window = session
                    .windows
                    .iter()
                    .find(|window| window.id == window_id)?;
                let session_is_current = selected_session
                    .as_deref()
                    .is_some_and(|selected| selected == session.id || selected == session.name);
                let window_is_current = selected_window.as_deref().map_or_else(
                    || session.active_window_id.as_deref() == Some(window_id),
                    |selected| selected == window_id,
                );
                Some((
                    session.id.clone(),
                    window
                        .anchor
                        .cwd
                        .clone()
                        .or_else(|| session.anchor.cwd.clone()),
                    session_is_current && window_is_current,
                ))
            })
        else {
            return false;
        };
        let live_terminal_cwd = target_is_current
            .then(|| self.terminal.current_working_directory().ok().flatten())
            .flatten();
        let cwd = terminal_cwd_for_mux_command(live_terminal_cwd, anchor_cwd);
        let config = self.multiplexer.clone();
        self.mux
            .execute_command(repaint, &config, MuxCommand::NewWindow { session_id, cwd });
        true
    }

    pub(super) fn reorder_window_before(
        &mut self,
        repaint: &RepaintHandle,
        source: &str,
        before: Option<&str>,
    ) -> bool {
        let Some(session_id) = self.mux.selected_session().map(str::to_owned) else {
            return false;
        };
        if before == Some(source) {
            return false;
        }
        let windows = self.mux.selected_session_windows();
        let Some(from) = windows.iter().position(|window| window.id == source) else {
            return false;
        };
        let mut target_ids = windows
            .iter()
            .map(|window| window.id.as_str())
            .filter(|id| *id != source)
            .collect::<Vec<_>>();
        let to = before
            .and_then(|before| target_ids.iter().position(|id| *id == before))
            .unwrap_or(target_ids.len());
        target_ids.insert(to, source);
        let Some(to) = target_ids.iter().position(|id| *id == source) else {
            return false;
        };
        let delta = to as i32 - from as i32;
        if delta == 0 {
            return false;
        }

        let config = self.multiplexer.clone();
        self.mux.execute_command(
            repaint,
            &config,
            MuxCommand::MoveWindow {
                session_id,
                window_id: Some(source.to_owned()),
                delta,
            },
        );
        true
    }

    pub(super) fn move_window(
        &mut self,
        repaint: &RepaintHandle,
        session_id: &str,
        window_id: &str,
        delta: i32,
    ) -> bool {
        let selected_session = self.mux.selected_session().map(str::to_owned);
        let selected_window = self.mux.selected_window().map(str::to_owned);
        let Some((session_id, position, window_count, active_window_id)) = self
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .and_then(|session| {
                let mut windows = session.windows.iter().collect::<Vec<_>>();
                windows.sort_by_key(|window| window.index);
                let active_window_id = (selected_session
                    .as_deref()
                    .is_some_and(|selected| selected == session.id || selected == session.name))
                .then_some(selected_window.as_deref())
                .flatten()
                .filter(|selected| windows.iter().any(|window| window.id == *selected))
                .map(str::to_owned)
                .or_else(|| session.active_window_id.clone());
                windows
                    .iter()
                    .position(|window| window.id == window_id)
                    .map(|position| {
                        (
                            session.id.clone(),
                            position,
                            windows.len(),
                            active_window_id,
                        )
                    })
            })
        else {
            return false;
        };
        let target = (position as i32 + delta).clamp(0, window_count as i32 - 1) as usize;
        if target == position {
            return false;
        }

        let command = match active_window_id {
            Some(selected_window_id) if selected_window_id.as_str() != window_id => {
                MuxCommand::MoveWindowPreservingSelection {
                    session_id,
                    window_id: window_id.to_owned(),
                    delta,
                    selected_window_id,
                }
            }
            _ => MuxCommand::MoveWindow {
                session_id,
                window_id: Some(window_id.to_owned()),
                delta,
            },
        };
        let config = self.multiplexer.clone();
        self.mux.execute_command(repaint, &config, command);
        true
    }

    pub(super) fn close_pane_for_window(
        &mut self,
        repaint: &RepaintHandle,
        session_id: &str,
        window_id: &str,
    ) -> bool {
        let Some((session_id, window_id, pane_id)) = self
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .and_then(|session| {
                session
                    .windows
                    .iter()
                    .find(|window| window.id == window_id)
                    .and_then(|window| {
                        window
                            .anchor
                            .pane_id
                            .clone()
                            .map(|pane_id| (session.id.clone(), window.id.clone(), pane_id))
                    })
            })
        else {
            return false;
        };
        let selected_session = self.mux.selected_session().map(str::to_owned);
        let current_window = self.current_window_id();
        let target_is_current = current_window.window_id == window_id
            && self
                .mux
                .sessions()
                .iter()
                .find(|session| session.id == session_id)
                .is_some_and(|session| {
                    selected_session
                        .as_deref()
                        .is_some_and(|selected| selected == session.id || selected == session.name)
                });
        let config = self.multiplexer.clone();
        self.mux
            .close_pane(&session_id, Some(&pane_id), repaint, &config);
        self.terminal.discard_pane(&pane_id);
        if self.uses_native_terminal_layout() {
            let window = self.window_id(session_id, window_id);
            self.remove_pane_from_layout(&window, &pane_id, target_is_current);
        }
        true
    }
}
