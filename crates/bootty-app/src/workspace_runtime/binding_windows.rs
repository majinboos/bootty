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

    pub(crate) fn reorder_window_before(
        &mut self,
        repaint: &RepaintHandle,
        source: &str,
        before: Option<&str>,
    ) -> bool {
        let Some(session_id) = self.mux.selected_session().map(str::to_owned) else {
            return false;
        };
        let windows = self.mux.selected_session_windows();
        let Some(from) = windows.iter().position(|window| window.id == source) else {
            return false;
        };
        let to = before
            .and_then(|before| windows.iter().position(|window| window.id == before))
            .unwrap_or(windows.len());
        let to = to - usize::from(to > from);
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
}
