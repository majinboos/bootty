#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TmuxParseError {
    FormatError,
    SyntaxError,
    ChecksumMismatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TmuxOutputNotification {
    pub pane_id: usize,
    pub data: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TmuxSessionChangedNotification {
    pub id: usize,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TmuxLayoutChangeNotification {
    pub window_id: usize,
    pub layout: String,
    pub visible_layout: String,
    pub raw_flags: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TmuxIdNameNotification {
    pub id: usize,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TmuxWindowPaneChangedNotification {
    pub window_id: usize,
    pub pane_id: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TmuxClientSessionChangedNotification {
    pub client: String,
    pub session_id: usize,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TmuxControlNotification {
    BlockEnd(String),
    BlockError(String),
    Output(TmuxOutputNotification),
    SessionChanged(TmuxSessionChangedNotification),
    SessionsChanged,
    LayoutChange(TmuxLayoutChangeNotification),
    WindowAdd { id: usize },
    WindowRenamed(TmuxIdNameNotification),
    WindowPaneChanged(TmuxWindowPaneChangedNotification),
    ClientDetached { client: String },
    ClientSessionChanged(TmuxClientSessionChangedNotification),
    Exit,
}

#[derive(Clone, Debug, Default)]
pub struct TmuxControlParser {
    line: String,
    block: Option<String>,
}

impl TmuxControlParser {
    pub fn put(
        &mut self,
        byte: u8,
    ) -> std::result::Result<Option<TmuxControlNotification>, TmuxParseError> {
        if byte != b'\n' {
            self.line.push(byte as char);
            return Ok(None);
        }

        let line = self.line.trim_end_matches('\r').to_owned();
        self.line.clear();
        self.parse_line(&line)
    }

    pub fn put_str(
        &mut self,
        input: &str,
    ) -> std::result::Result<Vec<TmuxControlNotification>, TmuxParseError> {
        let mut notifications = Vec::new();
        for byte in input.bytes() {
            if let Some(notification) = self.put(byte)? {
                notifications.push(notification);
            }
        }
        Ok(notifications)
    }

    fn parse_line(
        &mut self,
        line: &str,
    ) -> std::result::Result<Option<TmuxControlNotification>, TmuxParseError> {
        if let Some(block) = &mut self.block {
            if parse_tmux_block_terminator(line, "%end").is_some() {
                let payload = std::mem::take(block);
                self.block = None;
                return Ok(Some(TmuxControlNotification::BlockEnd(payload)));
            }
            if parse_tmux_block_terminator(line, "%error").is_some() {
                let payload = std::mem::take(block);
                self.block = None;
                return Ok(Some(TmuxControlNotification::BlockError(payload)));
            }
            if !block.is_empty() {
                block.push('\n');
            }
            block.push_str(line);
            return Ok(None);
        }

        if parse_tmux_block_terminator(line, "%begin").is_some() {
            self.block = Some(String::new());
            return Ok(None);
        }

        parse_tmux_control_notification(line).map(Some)
    }
}

fn parse_tmux_number(input: &str) -> std::result::Result<usize, TmuxParseError> {
    input
        .parse::<usize>()
        .map_err(|_| TmuxParseError::FormatError)
}

fn parse_prefixed_tmux_number(
    input: &str,
    prefix: char,
) -> std::result::Result<usize, TmuxParseError> {
    let value = input
        .strip_prefix(prefix)
        .filter(|value| !value.is_empty())
        .ok_or(TmuxParseError::FormatError)?;
    parse_tmux_number(value)
}

fn parse_tmux_block_terminator(line: &str, keyword: &str) -> Option<()> {
    let mut parts = line.split(' ');
    if parts.next() != Some(keyword) {
        return None;
    }
    for _ in 0..3 {
        parse_tmux_number(parts.next()?).ok()?;
    }
    parts.next().is_none().then_some(())
}

fn parse_tmux_control_notification(
    line: &str,
) -> std::result::Result<TmuxControlNotification, TmuxParseError> {
    if line == "%exit" || line == "%exit 0" {
        return Ok(TmuxControlNotification::Exit);
    }
    if line == "%sessions-changed" {
        return Ok(TmuxControlNotification::SessionsChanged);
    }

    let (kind, rest) = line.split_once(' ').unwrap_or((line, ""));
    match kind {
        "%output" => {
            let (pane, data) = split_tmux_control_rest(rest)?;
            Ok(TmuxControlNotification::Output(TmuxOutputNotification {
                pane_id: parse_prefixed_tmux_number(pane, '%')?,
                data: data.to_owned(),
            }))
        }
        "%session-changed" => {
            let (id, name) = split_tmux_control_rest(rest)?;
            Ok(TmuxControlNotification::SessionChanged(
                TmuxSessionChangedNotification {
                    id: parse_prefixed_tmux_number(id, '$')?,
                    name: name.to_owned(),
                },
            ))
        }
        "%layout-change" => {
            let parts = split_tmux_control_fields::<4>(rest)?;
            Ok(TmuxControlNotification::LayoutChange(
                TmuxLayoutChangeNotification {
                    window_id: parse_prefixed_tmux_number(parts[0], '@')?,
                    layout: parts[1].to_owned(),
                    visible_layout: parts[2].to_owned(),
                    raw_flags: parts[3].to_owned(),
                },
            ))
        }
        "%window-add" => Ok(TmuxControlNotification::WindowAdd {
            id: parse_prefixed_tmux_number(rest, '@')?,
        }),
        "%window-renamed" => {
            let (id, name) = split_tmux_control_rest(rest)?;
            Ok(TmuxControlNotification::WindowRenamed(
                TmuxIdNameNotification {
                    id: parse_prefixed_tmux_number(id, '@')?,
                    name: name.to_owned(),
                },
            ))
        }
        "%window-pane-changed" => {
            let parts = split_tmux_control_fields::<2>(rest)?;
            Ok(TmuxControlNotification::WindowPaneChanged(
                TmuxWindowPaneChangedNotification {
                    window_id: parse_prefixed_tmux_number(parts[0], '@')?,
                    pane_id: parse_prefixed_tmux_number(parts[1], '%')?,
                },
            ))
        }
        "%client-detached" => Ok(TmuxControlNotification::ClientDetached {
            client: rest.to_owned(),
        }),
        "%client-session-changed" => {
            let parts = split_tmux_control_fields::<3>(rest)?;
            Ok(TmuxControlNotification::ClientSessionChanged(
                TmuxClientSessionChangedNotification {
                    client: parts[0].to_owned(),
                    session_id: parse_prefixed_tmux_number(parts[1], '$')?,
                    name: parts[2].to_owned(),
                },
            ))
        }
        _ => Err(TmuxParseError::FormatError),
    }
}

fn split_tmux_control_rest(rest: &str) -> std::result::Result<(&str, &str), TmuxParseError> {
    rest.split_once(' ').ok_or(TmuxParseError::FormatError)
}

fn split_tmux_control_fields<const N: usize>(
    rest: &str,
) -> std::result::Result<[&str; N], TmuxParseError> {
    let mut fields = [""; N];
    let mut parts = rest.split(' ');
    for field in &mut fields {
        *field = parts.next().ok_or(TmuxParseError::FormatError)?;
    }
    if parts.next().is_some() {
        return Err(TmuxParseError::FormatError);
    }
    Ok(fields)
}
