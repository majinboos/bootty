use super::BindingRuntime;

use bootty_mux::{RepaintHandle, command::MuxCommand};

pub(crate) fn terminal_cwd_for_mux_command(
    live_terminal_cwd: Option<String>,
    anchor_cwd: Option<String>,
) -> Option<String> {
    live_terminal_cwd
        .and_then(|cwd| normalize_terminal_cwd(&cwd))
        .or(anchor_cwd)
}

fn normalize_terminal_cwd(cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    if let Some(path) = cwd.strip_prefix("file://") {
        let path_start = path.find('/')?;
        let path = &path[path_start..];
        return percent_decode(path);
    }
    Some(cwd.to_owned())
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hi = hex_value(*bytes.get(index + 1)?)?;
            let lo = hex_value(*bytes.get(index + 2)?)?;
            decoded.push((hi << 4) | lo);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

impl BindingRuntime {
    pub(crate) fn relative_session_id(&self, session_id: &str, delta: isize) -> Option<String> {
        let sessions = self.mux.sessions();
        let current = sessions
            .iter()
            .position(|session| session.id == session_id || session.name == session_id)?;
        let next = (current as isize + delta).rem_euclid(sessions.len() as isize) as usize;
        Some(sessions[next].id.clone())
    }

    pub(crate) fn previous_session_id(&self) -> Option<String> {
        self.mux.previous_selected_session().map(str::to_owned)
    }

    pub(crate) fn relative_window_target(
        &self,
        session_id: &str,
        window_id: &str,
        delta: isize,
    ) -> Option<(String, String)> {
        self.mux
            .session_by_id_or_name(session_id)
            .and_then(|session| {
                let mut windows = session.windows.iter().collect::<Vec<_>>();
                windows.sort_by_key(|window| window.index);
                let current = windows.iter().position(|window| window.id == window_id)?;
                let next = (current as isize + delta).rem_euclid(windows.len() as isize) as usize;
                Some((session.id.clone(), windows[next].id.clone()))
            })
    }

    pub(crate) fn activate_last_window(
        &mut self,
        repaint: &RepaintHandle,
        session_id: &str,
    ) -> bool {
        let Some(session_id) = self
            .mux
            .session_by_id_or_name(session_id)
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

    pub(crate) fn new_tab_for_window(
        &mut self,
        repaint: &RepaintHandle,
        session_id: &str,
        window_id: &str,
    ) -> bool {
        let selected_session = self.mux.selected_session().map(str::to_owned);
        let selected_window = self.mux.selected_window().map(str::to_owned);
        let Some((session_id, anchor_cwd, target_is_current)) = self
            .mux
            .session_by_id_or_name(session_id)
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

    pub(crate) fn reorder_window_before(
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

    pub(crate) fn move_window(
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
            .session_by_id_or_name(session_id)
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

    pub(crate) fn close_pane_for_window(
        &mut self,
        repaint: &RepaintHandle,
        session_id: &str,
        window_id: &str,
    ) -> bool {
        let Some((session_id, window_id, pane_id)) = self
            .mux
            .session_by_id_or_name(session_id)
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
