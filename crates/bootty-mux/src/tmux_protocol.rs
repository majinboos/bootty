use std::{
    collections::{BTreeMap, BTreeSet},
    process::Command,
};

use anyhow::{Context, Result, bail};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TmuxSession {
    pub name: String,
    pub attached: u32,
    pub windows: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TmuxLayout {
    pub width: usize,
    pub height: usize,
    pub x: usize,
    pub y: usize,
    pub content: TmuxLayoutContent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TmuxLayoutContent {
    Pane(usize),
    Horizontal(Vec<TmuxLayout>),
    Vertical(Vec<TmuxLayout>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TmuxParseError {
    MissingEntry,
    ExtraEntry,
    FormatError,
    SyntaxError,
    ChecksumMismatch,
}

macro_rules! tmux_output_variables {
    ($($variant:ident => $name:literal, $kind:expr;)+) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum TmuxOutputVariable {
            $($variant,)+
        }

        impl TmuxOutputVariable {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];

            pub fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)+
                }
            }

            fn kind(self) -> TmuxOutputValueKind {
                match self {
                    $(Self::$variant => $kind,)+
                }
            }

            pub fn parse(
                self,
                value: &str,
            ) -> std::result::Result<TmuxOutputValue, TmuxParseError> {
                parse_tmux_output_value(self.kind(), value)
            }
        }
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TmuxOutputValueKind {
    Bool,
    Number,
    String,
    PrefixedNumber(char),
}

tmux_output_variables! {
    AlternateOn => "alternate_on", TmuxOutputValueKind::Bool;
    AlternateSavedX => "alternate_saved_x", TmuxOutputValueKind::Number;
    AlternateSavedY => "alternate_saved_y", TmuxOutputValueKind::Number;
    BracketedPaste => "bracketed_paste", TmuxOutputValueKind::Bool;
    CursorBlinking => "cursor_blinking", TmuxOutputValueKind::Bool;
    CursorColour => "cursor_colour", TmuxOutputValueKind::String;
    CursorFlag => "cursor_flag", TmuxOutputValueKind::Bool;
    CursorShape => "cursor_shape", TmuxOutputValueKind::String;
    CursorX => "cursor_x", TmuxOutputValueKind::Number;
    CursorY => "cursor_y", TmuxOutputValueKind::Number;
    FocusFlag => "focus_flag", TmuxOutputValueKind::Bool;
    InsertFlag => "insert_flag", TmuxOutputValueKind::Bool;
    KeypadCursorFlag => "keypad_cursor_flag", TmuxOutputValueKind::Bool;
    KeypadFlag => "keypad_flag", TmuxOutputValueKind::Bool;
    MouseAllFlag => "mouse_all_flag", TmuxOutputValueKind::Bool;
    MouseAnyFlag => "mouse_any_flag", TmuxOutputValueKind::Bool;
    MouseButtonFlag => "mouse_button_flag", TmuxOutputValueKind::Bool;
    MouseSgrFlag => "mouse_sgr_flag", TmuxOutputValueKind::Bool;
    MouseStandardFlag => "mouse_standard_flag", TmuxOutputValueKind::Bool;
    MouseUtf8Flag => "mouse_utf8_flag", TmuxOutputValueKind::Bool;
    OriginFlag => "origin_flag", TmuxOutputValueKind::Bool;
    PaneId => "pane_id", TmuxOutputValueKind::PrefixedNumber('%');
    PaneTabs => "pane_tabs", TmuxOutputValueKind::String;
    ScrollRegionLower => "scroll_region_lower", TmuxOutputValueKind::Number;
    ScrollRegionUpper => "scroll_region_upper", TmuxOutputValueKind::Number;
    SessionId => "session_id", TmuxOutputValueKind::PrefixedNumber('$');
    Version => "version", TmuxOutputValueKind::String;
    WindowId => "window_id", TmuxOutputValueKind::PrefixedNumber('@');
    WindowWidth => "window_width", TmuxOutputValueKind::Number;
    WindowHeight => "window_height", TmuxOutputValueKind::Number;
    WindowLayout => "window_layout", TmuxOutputValueKind::String;
    WrapFlag => "wrap_flag", TmuxOutputValueKind::Bool;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TmuxOutputValue {
    Bool(bool),
    Number(usize),
    String(String),
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TmuxViewerWindow {
    pub id: usize,
    pub width: usize,
    pub height: usize,
    pub pane_ids: Vec<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TmuxViewerPane {
    pub output: String,
    pub cursor_x: usize,
    pub cursor_y: usize,
    pub cursor_visible: bool,
    pub wraparound: bool,
    pub insert: bool,
    pub origin: bool,
    pub keypad: bool,
    pub cursor_keys: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TmuxViewerAction {
    Exit,
    Command(String),
    Windows(Vec<TmuxViewerWindow>),
}

#[derive(Clone, Debug, Default)]
pub struct TmuxViewerState {
    pub session_id: Option<usize>,
    pub session_name: Option<String>,
    pub version: Option<String>,
    pub windows: Vec<TmuxViewerWindow>,
    pub panes: BTreeMap<usize, TmuxViewerPane>,
    pub command_queue: Vec<String>,
    pub exited: bool,
}

impl TmuxLayout {
    pub fn parse_with_checksum(input: &str) -> std::result::Result<Self, TmuxParseError> {
        if input.len() < 5 || input.as_bytes().get(4) != Some(&b',') {
            return Err(TmuxParseError::SyntaxError);
        }

        let layout = &input[5..];
        let checksum = tmux_layout_checksum(layout);
        if input[..4] != tmux_layout_checksum_string(checksum) {
            return Err(TmuxParseError::ChecksumMismatch);
        }

        Self::parse(layout)
    }

    pub fn parse(input: &str) -> std::result::Result<Self, TmuxParseError> {
        let mut parser = TmuxLayoutParser { input, offset: 0 };
        let layout = parser.parse_next()?;
        if parser.offset == input.len() {
            Ok(layout)
        } else {
            Err(TmuxParseError::SyntaxError)
        }
    }
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

impl TmuxViewerState {
    pub fn handle(&mut self, notification: TmuxControlNotification) -> Vec<TmuxViewerAction> {
        match notification {
            TmuxControlNotification::Exit => {
                if self.exited {
                    Vec::new()
                } else {
                    self.exited = true;
                    vec![TmuxViewerAction::Exit]
                }
            }
            TmuxControlNotification::SessionChanged(changed) => {
                let old_version = self.version.clone();
                self.session_id = Some(changed.id);
                self.session_name = Some(changed.name);
                self.version = old_version;
                self.windows.clear();
                self.panes.clear();
                self.command_queue.clear();
                let command = "display-message -p '#{version}'".to_owned();
                self.command_queue.push(command.clone());
                vec![
                    TmuxViewerAction::Windows(Vec::new()),
                    TmuxViewerAction::Command(command),
                ]
            }
            TmuxControlNotification::BlockEnd(payload) => self.handle_command_output(&payload),
            TmuxControlNotification::LayoutChange(change) => self.apply_layout_change(&change),
            TmuxControlNotification::WindowAdd { .. } => {
                let had_queue = !self.command_queue.is_empty();
                let command = "list-windows".to_owned();
                self.command_queue.push(command.clone());
                if had_queue {
                    Vec::new()
                } else {
                    vec![TmuxViewerAction::Command(command)]
                }
            }
            TmuxControlNotification::Output(output) => {
                if let Some(pane) = self.panes.get_mut(&output.pane_id) {
                    pane.output.push_str(&output.data);
                }
                Vec::new()
            }
            TmuxControlNotification::SessionsChanged
            | TmuxControlNotification::BlockError(_)
            | TmuxControlNotification::WindowRenamed(_)
            | TmuxControlNotification::WindowPaneChanged(_)
            | TmuxControlNotification::ClientDetached { .. }
            | TmuxControlNotification::ClientSessionChanged(_) => Vec::new(),
        }
    }

    fn handle_command_output(&mut self, payload: &str) -> Vec<TmuxViewerAction> {
        if !self.command_queue.is_empty() {
            self.command_queue.remove(0);
        }

        if self.version.is_none() && !payload.is_empty() && !payload.contains('\n') {
            self.version = Some(payload.to_owned());
            let command = "list-windows".to_owned();
            self.command_queue.push(command.clone());
            return vec![TmuxViewerAction::Command(command)];
        }

        if payload.starts_with('$') {
            return self.apply_windows_block(payload);
        }

        if payload.starts_with('%') && payload.contains(';') {
            self.apply_pane_state_block(payload);
        }

        Vec::new()
    }

    fn apply_windows_block(&mut self, payload: &str) -> Vec<TmuxViewerAction> {
        let mut windows = Vec::new();
        let mut pane_ids = BTreeSet::new();
        for line in payload.lines().filter(|line| !line.trim().is_empty()) {
            let mut parts = line.split_whitespace();
            let (Some(_), Some(id), Some(width), Some(height), Some(layout)) = (
                parts.next(),
                parts.next(),
                parts.next(),
                parts.next(),
                parts.next(),
            ) else {
                continue;
            };
            let window_id = match parse_prefixed_tmux_number(id, '@') {
                Ok(id) => id,
                Err(_) => continue,
            };
            let width = parse_tmux_number(width).unwrap_or(0);
            let height = parse_tmux_number(height).unwrap_or(0);
            let layout = match TmuxLayout::parse_with_checksum(layout) {
                Ok(layout) => layout,
                Err(_) => continue,
            };
            let mut window_panes = Vec::new();
            collect_layout_panes(&layout, &mut window_panes);
            pane_ids.extend(window_panes.iter().copied());
            windows.push(TmuxViewerWindow {
                id: window_id,
                width,
                height,
                pane_ids: window_panes,
            });
        }

        self.windows = windows;
        for pane_id in pane_ids {
            self.panes.entry(pane_id).or_default();
        }
        let command = self.queue_next_capture_command();
        let mut actions = vec![TmuxViewerAction::Windows(self.windows.clone())];
        if let Some(command) = command {
            actions.push(TmuxViewerAction::Command(command));
        }
        actions
    }

    fn apply_layout_change(
        &mut self,
        change: &TmuxLayoutChangeNotification,
    ) -> Vec<TmuxViewerAction> {
        let had_queue = !self.command_queue.is_empty();
        let Ok(layout) = TmuxLayout::parse_with_checksum(&change.layout) else {
            return Vec::new();
        };
        let mut pane_ids = Vec::new();
        collect_layout_panes(&layout, &mut pane_ids);
        for pane_id in &pane_ids {
            self.panes.entry(*pane_id).or_default();
        }
        if let Some(window) = self
            .windows
            .iter_mut()
            .find(|window| window.id == change.window_id)
        {
            window.pane_ids = pane_ids;
        }
        let command = self.queue_next_capture_command();
        let mut actions = vec![TmuxViewerAction::Windows(self.windows.clone())];
        if !had_queue && let Some(command) = command {
            actions.push(TmuxViewerAction::Command(command));
        }
        actions
    }

    fn apply_pane_state_block(&mut self, payload: &str) {
        for line in payload.lines() {
            let Some((pane_id, update)) = parse_tmux_pane_state_line(line) else {
                continue;
            };
            let pane = self.panes.entry(pane_id).or_default();
            update.apply(pane);
        }
    }

    fn queue_next_capture_command(&mut self) -> Option<String> {
        let pane_id = self
            .windows
            .iter()
            .flat_map(|window| &window.pane_ids)
            .next()
            .copied()?;
        let command = format!("capture-pane -p -t %{pane_id}");
        self.command_queue.push(command.clone());
        Some(command)
    }
}

pub fn tmux_output_format(variables: &[TmuxOutputVariable], delimiter: char) -> String {
    let delimiter_len = delimiter.len_utf8() * variables.len().saturating_sub(1);
    let variable_len = variables
        .iter()
        .map(|variable| variable.name().len() + 3)
        .sum::<usize>();
    let mut output = String::with_capacity(variable_len + delimiter_len);
    for (index, variable) in variables.iter().enumerate() {
        if index > 0 {
            output.push(delimiter);
        }
        output.push_str("#{");
        output.push_str(variable.name());
        output.push('}');
    }
    output
}

pub fn parse_tmux_output_values(
    variables: &[TmuxOutputVariable],
    input: &str,
    delimiter: char,
) -> std::result::Result<Vec<TmuxOutputValue>, TmuxParseError> {
    let mut parts = input.split(delimiter);
    let mut values = Vec::with_capacity(variables.len());
    for variable in variables {
        let part = parts.next().ok_or(TmuxParseError::MissingEntry)?;
        values.push(
            variable
                .parse(part)
                .map_err(|_| TmuxParseError::FormatError)?,
        );
    }
    if parts.next().is_some() {
        return Err(TmuxParseError::ExtraEntry);
    }
    Ok(values)
}

struct TmuxLayoutParser<'a> {
    input: &'a str,
    offset: usize,
}

impl TmuxLayoutParser<'_> {
    fn parse_next(&mut self) -> std::result::Result<TmuxLayout, TmuxParseError> {
        let width = self.read_number_until(b'x', true)?;
        let height = self.read_number_until(b',', true)?;
        let x = self.read_number_until(b',', true)?;
        let y = self.read_number_until_any(b",{[", false)?;
        let delimiter = *self
            .input
            .as_bytes()
            .get(self.offset)
            .ok_or(TmuxParseError::SyntaxError)?;

        let content = match delimiter {
            b',' => {
                self.offset += 1;
                let pane_id = self.read_number_until_any(b",}]", false)?;
                TmuxLayoutContent::Pane(pane_id)
            }
            b'{' | b'[' => {
                self.offset += 1;
                let mut children = Vec::new();
                loop {
                    children.push(self.parse_next()?);
                    let next = *self
                        .input
                        .as_bytes()
                        .get(self.offset)
                        .ok_or(TmuxParseError::SyntaxError)?;
                    if next == b',' {
                        self.offset += 1;
                        continue;
                    }

                    let expected = if delimiter == b'{' { b'}' } else { b']' };
                    if next != expected {
                        return Err(TmuxParseError::SyntaxError);
                    }
                    self.offset += 1;
                    break;
                }
                if delimiter == b'{' {
                    TmuxLayoutContent::Horizontal(children)
                } else {
                    TmuxLayoutContent::Vertical(children)
                }
            }
            _ => return Err(TmuxParseError::SyntaxError),
        };

        Ok(TmuxLayout {
            width,
            height,
            x,
            y,
            content,
        })
    }

    fn read_number_until(
        &mut self,
        delimiter: u8,
        consume: bool,
    ) -> std::result::Result<usize, TmuxParseError> {
        let rest = self
            .input
            .as_bytes()
            .get(self.offset..)
            .ok_or(TmuxParseError::SyntaxError)?;
        let index = rest
            .iter()
            .position(|byte| *byte == delimiter)
            .ok_or(TmuxParseError::SyntaxError)?;
        let number = parse_tmux_number(&self.input[self.offset..self.offset + index])
            .map_err(|_| TmuxParseError::SyntaxError)?;
        self.offset += index + usize::from(consume);
        Ok(number)
    }

    fn read_number_until_any(
        &mut self,
        delimiters: &[u8],
        consume: bool,
    ) -> std::result::Result<usize, TmuxParseError> {
        let rest = self
            .input
            .as_bytes()
            .get(self.offset..)
            .ok_or(TmuxParseError::SyntaxError)?;
        let index = rest
            .iter()
            .position(|byte| delimiters.contains(byte))
            .unwrap_or(rest.len());
        let number = parse_tmux_number(&self.input[self.offset..self.offset + index])
            .map_err(|_| TmuxParseError::SyntaxError)?;
        self.offset += index + usize::from(consume);
        Ok(number)
    }
}

pub fn tmux_layout_checksum(input: &str) -> u16 {
    tmux_layout_checksum_bytes(input.as_bytes())
}

pub fn tmux_layout_checksum_bytes(input: &[u8]) -> u16 {
    input.iter().fold(0u16, |checksum, byte| {
        checksum.rotate_right(1).wrapping_add(u16::from(*byte))
    })
}

pub fn tmux_layout_checksum_string(checksum: u16) -> String {
    format!("{checksum:04x}")
}

fn parse_tmux_number(input: &str) -> std::result::Result<usize, TmuxParseError> {
    input
        .parse::<usize>()
        .map_err(|_| TmuxParseError::FormatError)
}

fn parse_tmux_output_value(
    kind: TmuxOutputValueKind,
    value: &str,
) -> std::result::Result<TmuxOutputValue, TmuxParseError> {
    match kind {
        TmuxOutputValueKind::Bool => Ok(TmuxOutputValue::Bool(value == "1")),
        TmuxOutputValueKind::Number => parse_tmux_number(value).map(TmuxOutputValue::Number),
        TmuxOutputValueKind::String => Ok(TmuxOutputValue::String(value.to_owned())),
        TmuxOutputValueKind::PrefixedNumber(prefix) => {
            parse_prefixed_tmux_number(value, prefix).map(TmuxOutputValue::Number)
        }
    }
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TmuxPaneStateUpdate {
    cursor_x: usize,
    cursor_y: usize,
    cursor_visible: bool,
    wraparound: bool,
    insert: bool,
    origin: bool,
    keypad: bool,
    cursor_keys: bool,
}

impl TmuxPaneStateUpdate {
    fn apply(self, pane: &mut TmuxViewerPane) {
        pane.cursor_x = self.cursor_x;
        pane.cursor_y = self.cursor_y;
        pane.cursor_visible = self.cursor_visible;
        pane.wraparound = self.wraparound;
        pane.insert = self.insert;
        pane.origin = self.origin;
        pane.keypad = self.keypad;
        pane.cursor_keys = self.cursor_keys;
    }
}

fn parse_tmux_pane_state_line(line: &str) -> Option<(usize, TmuxPaneStateUpdate)> {
    let mut fields = line.split(';');
    let pane_id = parse_prefixed_tmux_number(fields.next()?, '%').ok()?;
    let mut update = TmuxPaneStateUpdate::default();
    let mut value_count = 0;

    for (index, field) in fields.enumerate() {
        value_count = index + 1;
        match index {
            0 => update.cursor_x = parse_tmux_number(field).unwrap_or(0),
            1 => update.cursor_y = parse_tmux_number(field).unwrap_or(0),
            2 => update.cursor_visible = field == "1",
            10 => update.wraparound = field == "1",
            11 => update.insert = field == "1",
            12 => update.origin = field == "1",
            13 => update.keypad = field == "1",
            14 => update.cursor_keys = field == "1",
            _ => {}
        }
    }

    (value_count >= 13).then_some((pane_id, update))
}

fn collect_layout_panes(layout: &TmuxLayout, panes: &mut Vec<usize>) {
    match &layout.content {
        TmuxLayoutContent::Pane(id) => panes.push(*id),
        TmuxLayoutContent::Horizontal(children) | TmuxLayoutContent::Vertical(children) => {
            for child in children {
                collect_layout_panes(child, panes);
            }
        }
    }
}

pub fn list_sessions() -> Result<Vec<TmuxSession>> {
    let output = Command::new("tmux")
        .args([
            "list-sessions",
            "-F",
            "#{session_name}\t#{session_attached}\t#{session_windows}",
        ])
        .output()
        .context("run tmux list-sessions")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("no server running") {
            return Ok(Vec::new());
        }
        bail!("tmux list-sessions failed: {}", stderr.trim());
    }

    parse_sessions(&String::from_utf8_lossy(&output.stdout))
}

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn switch_client(client_tty: &str, session_name: &str) -> Result<()> {
    let output = Command::new("tmux")
        .args(["switch-client", "-c", client_tty, "-t", session_name])
        .output()
        .context("run tmux switch-client")?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!("tmux switch-client failed: {}", stderr.trim());
}

pub fn client_session(client_tty: &str) -> Result<Option<String>> {
    let output = Command::new("tmux")
        .args(["list-clients", "-F", "#{client_tty}\t#{session_name}"])
        .output()
        .context("run tmux list-clients")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("no server running") {
            return Ok(None);
        }
        bail!("tmux list-clients failed: {}", stderr.trim());
    }

    Ok(parse_client_session(
        &String::from_utf8_lossy(&output.stdout),
        client_tty,
    ))
}

fn parse_client_session(output: &str, client_tty: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let (tty, session) = line.split_once('\t')?;
        (tty == client_tty).then(|| session.to_owned())
    })
}

fn parse_sessions(output: &str) -> Result<Vec<TmuxSession>> {
    let mut sessions = Vec::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.split('\t');
        let Some(name) = fields.next() else {
            continue;
        };
        let attached = fields
            .next()
            .unwrap_or("0")
            .parse()
            .with_context(|| format!("parse tmux attached count for {name}"))?;
        let windows = fields
            .next()
            .unwrap_or("0")
            .parse()
            .with_context(|| format!("parse tmux window count for {name}"))?;
        sessions.push(TmuxSession {
            name: name.to_owned(),
            attached,
            windows,
        });
    }
    Ok(sessions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tmux_sessions() {
        let sessions = parse_sessions("main\t1\t3\nwork\t0\t2\n").unwrap();
        assert_eq!(
            sessions,
            vec![
                TmuxSession {
                    name: "main".to_owned(),
                    attached: 1,
                    windows: 3,
                },
                TmuxSession {
                    name: "work".to_owned(),
                    attached: 0,
                    windows: 2,
                }
            ]
        );
    }

    #[test]
    fn shell_quotes_session_names() {
        assert_eq!(shell_quote("foo'bar"), "'foo'\\''bar'");
    }

    #[test]
    fn parses_tmux_client_session_by_tty() {
        let output = "/dev/ttys001\tmain\n/dev/ttys123\tarc/dblclick\n";

        assert_eq!(
            parse_client_session(output, "/dev/ttys123"),
            Some("arc/dblclick".to_owned())
        );
        assert_eq!(parse_client_session(output, "/dev/missing"), None);
    }

    #[test]
    fn tmux_layout_ports_tree_parse_checksum_and_syntax_cases() {
        let single = TmuxLayout::parse("80x24,0,0,42").unwrap();
        assert_eq!(single.width, 80);
        assert_eq!(single.height, 24);
        assert_eq!(single.x, 0);
        assert_eq!(single.y, 0);
        assert_eq!(single.content, TmuxLayoutContent::Pane(42));

        let offset = TmuxLayout::parse("40x12,10,5,7").unwrap();
        assert_eq!(
            (offset.width, offset.height, offset.x, offset.y),
            (40, 12, 10, 5)
        );
        assert_eq!(offset.content, TmuxLayoutContent::Pane(7));

        let large = TmuxLayout::parse("1920x1080,100,200,999").unwrap();
        assert_eq!(
            (large.width, large.height, large.x, large.y),
            (1920, 1080, 100, 200)
        );
        assert_eq!(large.content, TmuxLayoutContent::Pane(999));

        let horizontal = TmuxLayout::parse("80x24,0,0{40x24,0,0,1,40x24,40,0,2}").unwrap();
        let TmuxLayoutContent::Horizontal(children) = horizontal.content else {
            panic!("horizontal split");
        };
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].content, TmuxLayoutContent::Pane(1));
        assert_eq!(
            (children[1].width, children[1].height, children[1].x),
            (40, 24, 40)
        );
        assert_eq!(children[1].content, TmuxLayoutContent::Pane(2));

        let vertical = TmuxLayout::parse("80x24,0,0[80x12,0,0,1,80x12,0,12,2]").unwrap();
        let TmuxLayoutContent::Vertical(children) = vertical.content else {
            panic!("vertical split");
        };
        assert_eq!(children.len(), 2);
        assert_eq!(
            (children[0].width, children[0].height, children[0].y),
            (80, 12, 0)
        );
        assert_eq!(
            (children[1].width, children[1].height, children[1].y),
            (80, 12, 12)
        );

        let three = TmuxLayout::parse("120x24,0,0{40x24,0,0,1,40x24,40,0,2,40x24,80,0,3}").unwrap();
        let TmuxLayoutContent::Horizontal(children) = three.content else {
            panic!("three pane split");
        };
        assert_eq!(
            children
                .iter()
                .map(|child| child.content.clone())
                .collect::<Vec<_>>(),
            vec![
                TmuxLayoutContent::Pane(1),
                TmuxLayoutContent::Pane(2),
                TmuxLayoutContent::Pane(3)
            ]
        );

        let nested =
            TmuxLayout::parse("80x24,0,0[80x12,0,0,1,80x12,0,12{40x12,0,12,2,40x12,40,12,3}]")
                .unwrap();
        let TmuxLayoutContent::Vertical(children) = nested.content else {
            panic!("nested vertical");
        };
        assert_eq!(children[0].content, TmuxLayoutContent::Pane(1));
        let TmuxLayoutContent::Horizontal(bottom) = &children[1].content else {
            panic!("nested horizontal");
        };
        assert_eq!(bottom[0].content, TmuxLayoutContent::Pane(2));
        assert_eq!(bottom[1].content, TmuxLayoutContent::Pane(3));

        let nested =
            TmuxLayout::parse("80x24,0,0{40x24,0,0,1,40x24,40,0[40x12,40,0,2,40x12,40,12,3]}")
                .unwrap();
        let TmuxLayoutContent::Horizontal(children) = nested.content else {
            panic!("nested horizontal");
        };
        let TmuxLayoutContent::Vertical(right) = &children[1].content else {
            panic!("nested vertical");
        };
        assert_eq!(right[0].content, TmuxLayoutContent::Pane(2));
        assert_eq!(right[1].content, TmuxLayoutContent::Pane(3));

        let deep = TmuxLayout::parse("80x24,0,0{40x24,0,0[40x12,0,0,1,40x12,0,12,2],40x24,40,0,3}")
            .unwrap();
        let TmuxLayoutContent::Horizontal(children) = deep.content else {
            panic!("deep horizontal");
        };
        let TmuxLayoutContent::Vertical(left) = &children[0].content else {
            panic!("deep vertical");
        };
        assert_eq!(left[0].content, TmuxLayoutContent::Pane(1));
        assert_eq!(left[1].content, TmuxLayoutContent::Pane(2));
        assert_eq!(children[1].content, TmuxLayoutContent::Pane(3));

        for bad in [
            "",
            "x24,0,0,1",
            "80x,0,0,1",
            "80x24,,0,1",
            "80x24,0,,1",
            "80x24,0,0,",
            "abcx24,0,0,1",
            "80x24,0,0,abc",
            "80x24,0,0{40x24,0,0,1",
            "80x24,0,0[40x24,0,0,1",
            "80x24,0,0{40x24,0,0,1]",
            "80x24,0,0[40x24,0,0,1}",
            "80x24,0,0,1extra",
            "8024,0,0,1",
            "80x24,0,0",
        ] {
            assert_eq!(
                TmuxLayout::parse(bad),
                Err(TmuxParseError::SyntaxError),
                "{bad}"
            );
        }

        let with_checksum =
            TmuxLayout::parse_with_checksum("f8f9,80x24,0,0{40x24,0,0,1,40x24,40,0,2}").unwrap();
        assert_eq!((with_checksum.width, with_checksum.height), (80, 24));
        assert_eq!(
            TmuxLayout::parse_with_checksum("0000,80x24,0,0{40x24,0,0,1,40x24,40,0,2}"),
            Err(TmuxParseError::ChecksumMismatch)
        );
        assert_eq!(
            TmuxLayout::parse_with_checksum("bb62"),
            Err(TmuxParseError::SyntaxError)
        );
        assert_eq!(
            TmuxLayout::parse_with_checksum("bb62x159x48,0,0"),
            Err(TmuxParseError::SyntaxError)
        );

        assert_eq!(tmux_layout_checksum(""), 0);
        assert_eq!(
            tmux_layout_checksum_string(tmux_layout_checksum("A")),
            "0041"
        );
        assert_eq!(
            tmux_layout_checksum_string(tmux_layout_checksum("AB")),
            "8062"
        );
        assert_eq!(
            tmux_layout_checksum_string(tmux_layout_checksum("80x24,0,0,42")),
            "d962"
        );
        assert_eq!(
            tmux_layout_checksum_string(tmux_layout_checksum(
                "80x24,0,0{40x24,0,0,1,40x24,40,0,2}"
            )),
            "f8f9"
        );
        assert_eq!(tmux_layout_checksum_string(0x000f), "000f");
        assert_eq!(tmux_layout_checksum_string(0x1234), "1234");
        assert_eq!(tmux_layout_checksum_string(0xabcd), "abcd");
        assert_eq!(tmux_layout_checksum_string(0xffff), "ffff");
        assert_eq!(
            tmux_layout_checksum_string(tmux_layout_checksum_bytes(&[0xff; 8])),
            "03fc"
        );
        assert_eq!(
            tmux_layout_checksum_string(tmux_layout_checksum("159x48,0,0{79x48,0,0,79x48,80,0}")),
            "bb62"
        );
        assert_eq!(
            tmux_layout_checksum("159x48,0,0{79x48,0,0,79x48,80,0}"),
            tmux_layout_checksum("159x48,0,0{79x48,0,0,79x48,80,0}")
        );
        assert_ne!(
            tmux_layout_checksum("80x24,0,0,1"),
            tmux_layout_checksum("80x24,0,0,2")
        );
    }

    #[test]
    fn tmux_output_ports_variable_parse_and_format_cases() {
        for variable in TmuxOutputVariable::ALL {
            match variable.kind() {
                TmuxOutputValueKind::Bool => {
                    assert_eq!(variable.parse("1").unwrap(), TmuxOutputValue::Bool(true));
                    for value in ["0", "", "true"] {
                        assert_eq!(variable.parse(value).unwrap(), TmuxOutputValue::Bool(false));
                    }
                }
                TmuxOutputValueKind::Number => {
                    for (value, expected) in [("0", 0), ("42", 42)] {
                        assert_eq!(
                            variable.parse(value).unwrap(),
                            TmuxOutputValue::Number(expected)
                        );
                    }
                    for value in ["abc", "80px", "-1"] {
                        assert_eq!(variable.parse(value), Err(TmuxParseError::FormatError));
                    }
                }
                TmuxOutputValueKind::String => {
                    for value in [
                        "red",
                        "#ff0000",
                        "0,8,16,24",
                        "next-3.5",
                        "",
                        "a]b,c{d}e(f)",
                    ] {
                        assert_eq!(
                            variable.parse(value).unwrap(),
                            TmuxOutputValue::String(value.to_owned())
                        );
                    }
                }
                TmuxOutputValueKind::PrefixedNumber(prefix) => {
                    for (value, expected) in
                        [(format!("{prefix}42"), 42), (format!("{prefix}0"), 0)]
                    {
                        assert_eq!(
                            variable.parse(&value).unwrap(),
                            TmuxOutputValue::Number(expected)
                        );
                    }
                    for value in [
                        "0".to_owned(),
                        "$".to_owned(),
                        String::new(),
                        format!("{prefix}abc"),
                    ] {
                        assert_eq!(variable.parse(&value), Err(TmuxParseError::FormatError));
                    }
                }
            }
        }
        assert_eq!(
            TmuxOutputVariable::WindowId.parse("@12345").unwrap(),
            TmuxOutputValue::Number(12345)
        );

        assert_eq!(
            parse_tmux_output_values(&[TmuxOutputVariable::SessionId], "$42", ' ').unwrap(),
            vec![TmuxOutputValue::Number(42)]
        );
        assert_eq!(
            parse_tmux_output_values(
                &[
                    TmuxOutputVariable::SessionId,
                    TmuxOutputVariable::WindowId,
                    TmuxOutputVariable::WindowWidth,
                    TmuxOutputVariable::WindowHeight,
                ],
                "$1 @2 80 24",
                ' ',
            )
            .unwrap(),
            vec![
                TmuxOutputValue::Number(1),
                TmuxOutputValue::Number(2),
                TmuxOutputValue::Number(80),
                TmuxOutputValue::Number(24),
            ]
        );
        assert_eq!(
            parse_tmux_output_values(
                &[
                    TmuxOutputVariable::WindowId,
                    TmuxOutputVariable::WindowLayout
                ],
                "@5,abc123",
                ',',
            )
            .unwrap(),
            vec![
                TmuxOutputValue::Number(5),
                TmuxOutputValue::String("abc123".to_owned()),
            ]
        );
        assert_eq!(
            parse_tmux_output_values(
                &[
                    TmuxOutputVariable::WindowWidth,
                    TmuxOutputVariable::WindowHeight
                ],
                "120\t40",
                '\t',
            )
            .unwrap(),
            vec![TmuxOutputValue::Number(120), TmuxOutputValue::Number(40)]
        );
        assert_eq!(
            parse_tmux_output_values(
                &[TmuxOutputVariable::SessionId, TmuxOutputVariable::WindowId],
                "$1",
                ' ',
            ),
            Err(TmuxParseError::MissingEntry)
        );
        assert_eq!(
            parse_tmux_output_values(&[TmuxOutputVariable::SessionId], "$1 @2", ' '),
            Err(TmuxParseError::ExtraEntry)
        );
        for bad in ["42", "@42", "$abc", ""] {
            assert_eq!(
                parse_tmux_output_values(&[TmuxOutputVariable::SessionId], bad, ' '),
                Err(TmuxParseError::FormatError)
            );
        }
        assert_eq!(
            parse_tmux_output_values(
                &[
                    TmuxOutputVariable::SessionId,
                    TmuxOutputVariable::WindowLayout
                ],
                "$1,",
                ',',
            )
            .unwrap(),
            vec![
                TmuxOutputValue::Number(1),
                TmuxOutputValue::String(String::new()),
            ]
        );

        assert_eq!(
            tmux_output_format(&[TmuxOutputVariable::SessionId], ' '),
            "#{session_id}"
        );
        assert_eq!(
            tmux_output_format(
                &[
                    TmuxOutputVariable::SessionId,
                    TmuxOutputVariable::WindowId,
                    TmuxOutputVariable::WindowWidth,
                    TmuxOutputVariable::WindowHeight,
                ],
                ' ',
            ),
            "#{session_id} #{window_id} #{window_width} #{window_height}"
        );
        assert_eq!(
            tmux_output_format(
                &[
                    TmuxOutputVariable::WindowId,
                    TmuxOutputVariable::WindowLayout
                ],
                ',',
            ),
            "#{window_id},#{window_layout}"
        );
        assert_eq!(
            tmux_output_format(
                &[
                    TmuxOutputVariable::WindowWidth,
                    TmuxOutputVariable::WindowHeight
                ],
                '\t',
            ),
            "#{window_width}\t#{window_height}"
        );
        assert_eq!(tmux_output_format(&[], ' '), "");
        assert_eq!(
            tmux_output_format(
                &[
                    TmuxOutputVariable::SessionId,
                    TmuxOutputVariable::WindowId,
                    TmuxOutputVariable::WindowWidth,
                    TmuxOutputVariable::WindowHeight,
                    TmuxOutputVariable::WindowLayout,
                ],
                ' ',
            ),
            "#{session_id} #{window_id} #{window_width} #{window_height} #{window_layout}"
        );
    }

    #[test]
    fn tmux_control_ports_block_and_notification_cases() {
        fn feed(input: &str) -> Vec<TmuxControlNotification> {
            TmuxControlParser::default().put_str(input).unwrap()
        }

        assert_eq!(
            feed("%begin 1578922740 269 1\n%end 1578922740 269 1\n"),
            vec![TmuxControlNotification::BlockEnd(String::new())]
        );
        assert_eq!(
            feed("%begin 1578922740 269 1\n%error 1578922740 269 1\n"),
            vec![TmuxControlNotification::BlockError(String::new())]
        );
        assert_eq!(
            feed("%begin 1578922740 269 1\nhello\nworld\n%end 1578922740 269 1\n"),
            vec![TmuxControlNotification::BlockEnd("hello\nworld".to_owned())]
        );
        assert_eq!(
            feed("%begin 1 1 1\n%end not really\nhello\n%end 1 1 1\n"),
            vec![TmuxControlNotification::BlockEnd(
                "%end not really\nhello".to_owned()
            )]
        );
        assert_eq!(
            feed("%begin 1 1 1\n%error not really\nhello\n%end 1 1 1\n"),
            vec![TmuxControlNotification::BlockEnd(
                "%error not really\nhello".to_owned()
            )]
        );
        assert_eq!(
            feed("%begin 1 1 1\n%error not really\nhello\n%error 1 1 1\n"),
            vec![TmuxControlNotification::BlockError(
                "%error not really\nhello".to_owned()
            )]
        );
        assert_eq!(
            feed("%begin 1 1 1\n%end 1 1 1 trailing\nhello\n%end 1 1 1\n"),
            vec![TmuxControlNotification::BlockEnd(
                "%end 1 1 1 trailing\nhello".to_owned()
            )]
        );
        assert_eq!(
            feed("%begin 1 1 1\n%end foo bar baz\nhello\n%end 1 1 1\n"),
            vec![TmuxControlNotification::BlockEnd(
                "%end foo bar baz\nhello".to_owned()
            )]
        );

        assert_eq!(
            feed("%output %42 foo bar baz\n"),
            vec![TmuxControlNotification::Output(TmuxOutputNotification {
                pane_id: 42,
                data: "foo bar baz".to_owned(),
            })]
        );
        assert_eq!(
            feed("%session-changed $42 foo\n"),
            vec![TmuxControlNotification::SessionChanged(
                TmuxSessionChangedNotification {
                    id: 42,
                    name: "foo".to_owned(),
                }
            )]
        );
        assert_eq!(
            feed("%sessions-changed\r\n"),
            vec![TmuxControlNotification::SessionsChanged]
        );
        assert_eq!(
            feed(
                "%layout-change @2 1234x791,0,0{617x791,0,0,0,617x791,618,0,1} 1234x791,0,0{617x791,0,0,0,617x791,618,0,1} *-\n"
            ),
            vec![TmuxControlNotification::LayoutChange(
                TmuxLayoutChangeNotification {
                    window_id: 2,
                    layout: "1234x791,0,0{617x791,0,0,0,617x791,618,0,1}".to_owned(),
                    visible_layout: "1234x791,0,0{617x791,0,0,0,617x791,618,0,1}".to_owned(),
                    raw_flags: "*-".to_owned(),
                }
            )]
        );
        assert_eq!(
            feed("%window-add @14\n"),
            vec![TmuxControlNotification::WindowAdd { id: 14 }]
        );
        assert_eq!(
            feed("%window-renamed @42 bar\n"),
            vec![TmuxControlNotification::WindowRenamed(
                TmuxIdNameNotification {
                    id: 42,
                    name: "bar".to_owned(),
                }
            )]
        );
        assert_eq!(
            feed("%window-pane-changed @42 %2\n"),
            vec![TmuxControlNotification::WindowPaneChanged(
                TmuxWindowPaneChangedNotification {
                    window_id: 42,
                    pane_id: 2,
                }
            )]
        );
        assert_eq!(
            feed("%client-detached /dev/pts/1\n"),
            vec![TmuxControlNotification::ClientDetached {
                client: "/dev/pts/1".to_owned(),
            }]
        );
        assert_eq!(
            feed("%client-session-changed /dev/pts/1 $2 mysession\n"),
            vec![TmuxControlNotification::ClientSessionChanged(
                TmuxClientSessionChangedNotification {
                    client: "/dev/pts/1".to_owned(),
                    session_id: 2,
                    name: "mysession".to_owned(),
                }
            )]
        );
    }

    #[test]
    fn tmux_viewer_ports_startup_layout_queue_and_pane_state_cases() {
        let mut viewer = TmuxViewerState::default();
        assert_eq!(
            viewer.handle(TmuxControlNotification::Exit),
            vec![TmuxViewerAction::Exit]
        );
        assert_eq!(viewer.handle(TmuxControlNotification::Exit), Vec::new());

        let mut viewer = TmuxViewerState::default();
        assert!(
            viewer
                .handle(TmuxControlNotification::BlockEnd(String::new()))
                .is_empty()
        );
        let actions = viewer.handle(TmuxControlNotification::SessionChanged(
            TmuxSessionChangedNotification {
                id: 42,
                name: "main".to_owned(),
            },
        ));
        assert_eq!(viewer.session_id, Some(42));
        assert!(actions.iter().any(|action| {
            matches!(action, TmuxViewerAction::Command(command) if command.contains("display-message"))
        }));

        let actions = viewer.handle(TmuxControlNotification::BlockEnd("3.5a".to_owned()));
        assert_eq!(viewer.version.as_deref(), Some("3.5a"));
        assert!(actions.iter().any(|action| {
            matches!(action, TmuxViewerAction::Command(command) if command.contains("list-windows"))
        }));

        let actions = viewer.handle(TmuxControlNotification::BlockEnd(
            "$0 @0 83 44 027b,83x44,0,0[83x20,0,0,0,83x23,0,21,1]".to_owned(),
        ));
        assert_eq!(viewer.windows.len(), 1);
        assert_eq!(viewer.windows[0].pane_ids, vec![0, 1]);
        assert_eq!(viewer.panes.len(), 2);
        assert!(
            actions
                .iter()
                .any(|action| matches!(action, TmuxViewerAction::Windows(_)))
        );
        assert!(actions.iter().any(|action| {
            matches!(action, TmuxViewerAction::Command(command) if command.contains("capture-pane") && command.contains("%0"))
        }));

        viewer.handle(TmuxControlNotification::BlockEnd(
            "Hello, world!".to_owned(),
        ));
        viewer.handle(TmuxControlNotification::Output(TmuxOutputNotification {
            pane_id: 0,
            data: "new output".to_owned(),
        }));
        viewer.handle(TmuxControlNotification::Output(TmuxOutputNotification {
            pane_id: 999,
            data: "ignored".to_owned(),
        }));
        assert!(viewer.panes.get(&0).unwrap().output.contains("new output"));
        assert!(!viewer.panes.contains_key(&999));

        let actions = viewer.handle(TmuxControlNotification::LayoutChange(
            TmuxLayoutChangeNotification {
                window_id: 0,
                layout: "e07b,83x44,0,0[83x22,0,0,0,83x21,0,23,2]".to_owned(),
                visible_layout: "e07b,83x44,0,0[83x22,0,0,0,83x21,0,23,2]".to_owned(),
                raw_flags: "*".to_owned(),
            },
        ));
        assert_eq!(viewer.windows[0].pane_ids, vec![0, 2]);
        assert!(viewer.panes.contains_key(&2));
        assert!(
            actions
                .iter()
                .any(|action| matches!(action, TmuxViewerAction::Windows(_)))
        );

        let queue_len = viewer.command_queue.len();
        let actions = viewer.handle(TmuxControlNotification::WindowAdd { id: 1 });
        assert!(viewer.command_queue.len() > queue_len);
        assert!(actions.is_empty());

        while !viewer.command_queue.is_empty() {
            viewer.handle(TmuxControlNotification::BlockEnd(String::new()));
        }
        let actions = viewer.handle(TmuxControlNotification::WindowAdd { id: 2 });
        assert!(actions.iter().any(|action| {
            matches!(action, TmuxViewerAction::Command(command) if command.contains("list-windows"))
        }));

        let actions = viewer.handle(TmuxControlNotification::SessionChanged(
            TmuxSessionChangedNotification {
                id: 2,
                name: "second".to_owned(),
            },
        ));
        assert_eq!(viewer.session_id, Some(2));
        assert_eq!(viewer.version.as_deref(), Some("3.5a"));
        assert!(viewer.windows.is_empty());
        assert!(viewer.panes.is_empty());
        assert!(actions.iter().any(
            |action| matches!(action, TmuxViewerAction::Windows(windows) if windows.is_empty())
        ));

        viewer.handle(TmuxControlNotification::BlockEnd(
            "$2 @1 165 79 ca97,165x79,0,0[165x40,0,0,0,165x38,0,41,4]".to_owned(),
        ));
        assert_eq!(viewer.windows[0].id, 1);
        assert_eq!(viewer.windows[0].pane_ids, vec![0, 4]);

        viewer.handle(TmuxControlNotification::BlockEnd(
            "%0;42;0;1;;;;0;4294967295;4294967295;0;1;0;0;0;0;0;0;0;0;0;;;0;39;8,16\n%4;10;5;1;;;;0;4294967295;4294967295;0;1;0;0;0;0;0;0;0;0;0;;;0;37;8,16"
                .to_owned(),
        ));
        let pane_0 = viewer.panes.get(&0).unwrap();
        assert_eq!((pane_0.cursor_x, pane_0.cursor_y), (42, 0));
        assert!(pane_0.cursor_visible);
        assert!(pane_0.wraparound);
        assert!(!pane_0.insert);
        assert!(!pane_0.origin);
        assert!(!pane_0.keypad);
        assert!(!pane_0.cursor_keys);

        let pane_4 = viewer.panes.get(&4).unwrap();
        assert_eq!((pane_4.cursor_x, pane_4.cursor_y), (10, 5));
        assert!(pane_4.cursor_visible);
    }

    #[test]
    fn tmux_module_ports_public_terminal_tmux_surfaces() {
        let _layout = TmuxLayout::parse("80x24,0,0,42").unwrap();
        let mut parser = TmuxControlParser::default();
        assert_eq!(
            parser.put_str("%sessions-changed\n").unwrap(),
            vec![TmuxControlNotification::SessionsChanged]
        );
        let mut viewer = TmuxViewerState::default();
        assert_eq!(
            viewer.handle(TmuxControlNotification::Exit),
            vec![TmuxViewerAction::Exit]
        );
        assert_eq!(
            tmux_output_format(&[TmuxOutputVariable::SessionId], ' '),
            "#{session_id}"
        );
    }
}
