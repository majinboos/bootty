use std::{cell::RefCell, rc::Rc, sync::mpsc::Sender};

use base64::{Engine as _, engine::general_purpose};
use memchr::memmem::find;

use crate::terminal_engine::write_ingress::{find_osc_terminator, split_osc_payload};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalSideEffect {
    Bell,
    ClipboardWrite(String),
    ClipboardQuery { selection: String },
    WindowTitle(String),
    WindowIcon(String),
    DesktopNotification { title: String, body: String },
    MouseShape(String),
    SemanticPrompt(String),
    KittyTextSizing(String),
    ConEmuControl(String),
    ConEmuProgress { state: String, value: Option<u8> },
    Iterm2UserVarPorts(Vec<u16>),
    Iterm2Control(String),
    Iterm2File(String),
    OpenUrl(String),
    FocusWindow,
    ReportCellSize,
    ReportVariable(String),
    UnsupportedHostCommand { protocol: String, command: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalSideEffectEvent {
    pub source_pane_id: Option<String>,
    pub effect: TerminalSideEffect,
}

impl TerminalSideEffectEvent {
    pub fn new(source_pane_id: Option<String>, effect: TerminalSideEffect) -> Self {
        Self {
            source_pane_id,
            effect,
        }
    }
}

pub fn deliver_terminal_side_effects(
    sender: &mut Option<Sender<TerminalSideEffectEvent>>,
    source_pane_id: &Option<String>,
    effects: Vec<TerminalSideEffect>,
) {
    let Some(active_sender) = sender.as_ref() else {
        return;
    };
    for effect in effects {
        let event = TerminalSideEffectEvent::new(source_pane_id.clone(), effect);
        if active_sender.send(event).is_err() {
            *sender = None;
            return;
        }
    }
}

pub(crate) enum TerminalHostAction {
    WriteVt(&'static [u8]),
}

pub(crate) struct TerminalSideEffectCollector {
    osc_pending: Vec<u8>,
    effects: Vec<TerminalSideEffect>,
    callback_effects: Rc<RefCell<Vec<TerminalSideEffect>>>,
    iterm_copy_capture: Option<ItermCopyCapture>,
}

#[derive(Default)]
struct ItermCopyCapture {
    text: Vec<u8>,
    escape: ItermCopyEscape,
}

#[derive(Clone, Copy, Default)]
enum ItermCopyEscape {
    #[default]
    None,
    Escape,
    Intermediate,
    Csi,
    String,
    StringEscape,
}

impl TerminalSideEffectCollector {
    pub(crate) fn new() -> Self {
        Self {
            osc_pending: Vec::new(),
            effects: Vec::new(),
            callback_effects: Rc::new(RefCell::new(Vec::new())),
            iterm_copy_capture: None,
        }
    }

    pub(crate) fn callback_effects(&self) -> Rc<RefCell<Vec<TerminalSideEffect>>> {
        self.callback_effects.clone()
    }

    pub(crate) fn needs_input(&self) -> bool {
        !self.osc_pending.is_empty() || self.iterm_copy_capture.is_some()
    }

    pub(crate) fn collect(&mut self, data: &[u8]) -> Vec<TerminalHostAction> {
        let mut actions = Vec::new();
        let mut bytes = Vec::with_capacity(self.osc_pending.len() + data.len());
        bytes.extend_from_slice(&self.osc_pending);
        bytes.extend_from_slice(data);
        self.osc_pending.clear();

        let mut search_start = 0;
        while let Some(relative_start) = find(&bytes[search_start..], b"\x1b]") {
            let start = search_start + relative_start;
            if start > search_start {
                self.append_iterm_copy_text(&bytes[search_start..start]);
            }
            let payload_start = start + 2;
            match find_osc_terminator(&bytes[payload_start..]) {
                Some((payload_len, terminator_len)) => {
                    let payload = &bytes[payload_start..payload_start + payload_len];
                    self.push_osc_side_effect(payload, &mut actions);
                    search_start = payload_start + payload_len + terminator_len;
                }
                None => {
                    self.osc_pending.extend_from_slice(&bytes[start..]);
                    return actions;
                }
            }
        }
        if search_start < bytes.len() {
            self.append_iterm_copy_text(&bytes[search_start..]);
        }
        actions
    }

    pub(crate) fn drain(&mut self) -> Vec<TerminalSideEffect> {
        self.effects.append(&mut self.callback_effects.borrow_mut());
        std::mem::take(&mut self.effects)
    }

    fn push_osc_side_effect(&mut self, payload: &[u8], actions: &mut Vec<TerminalHostAction>) {
        let Some((command, rest)) = split_osc_payload(payload) else {
            return;
        };
        match command {
            b"1" => self.push_utf8_effect(rest, TerminalSideEffect::WindowIcon),
            b"9" => {
                if let Ok(data) = std::str::from_utf8(rest) {
                    if is_conemu_osc9(data) {
                        self.push_conemu_side_effect(data);
                    } else {
                        self.effects.push(TerminalSideEffect::DesktopNotification {
                            title: String::new(),
                            body: data.to_owned(),
                        });
                    }
                }
            }
            b"22" => self.push_utf8_effect(rest, TerminalSideEffect::MouseShape),
            b"52" => self.effects.extend(osc52_side_effect(rest)),
            b"133" => self.push_utf8_effect(rest, TerminalSideEffect::SemanticPrompt),
            b"66" => self.push_utf8_effect(rest, TerminalSideEffect::KittyTextSizing),
            b"777" => self.push_osc777_side_effect(rest),
            b"1337" => {
                if let Ok(data) = std::str::from_utf8(rest) {
                    self.push_iterm2_side_effect(data, actions);
                }
            }
            _ => {}
        }
    }

    fn push_utf8_effect(&mut self, payload: &[u8], effect: fn(String) -> TerminalSideEffect) {
        if let Ok(text) = std::str::from_utf8(payload) {
            self.effects.push(effect(text.to_owned()));
        }
    }

    fn push_iterm2_control(&mut self, data: &str) {
        self.effects
            .push(TerminalSideEffect::Iterm2Control(data.to_owned()));
    }

    fn append_iterm_copy_text(&mut self, data: &[u8]) {
        let Some(capture) = self.iterm_copy_capture.as_mut() else {
            return;
        };
        capture.append_plain_text(data);
    }

    fn push_conemu_side_effect(&mut self, data: &str) {
        let mut parts = data.split(';');
        let kind = parts.next().unwrap_or_default();
        match kind {
            "2" => self.effects.push(TerminalSideEffect::WindowTitle(
                parts.collect::<Vec<_>>().join(";"),
            )),
            "4" => {
                let first = parts.next().unwrap_or_default();
                let second = parts.next();
                let (state, value) = match second {
                    Some(value) => (conemu_progress_state(first), value.parse::<u8>().ok()),
                    None if matches!(first, "0" | "1" | "2" | "3" | "4") => {
                        (conemu_progress_state(first), None)
                    }
                    None => ("normal", first.parse::<u8>().ok()),
                };
                self.effects.push(TerminalSideEffect::ConEmuProgress {
                    state: state.to_owned(),
                    value: value.map(|value| value.min(100)),
                });
            }
            "6" => self.effects.push(TerminalSideEffect::SemanticPrompt(
                "conemu-prompt".to_owned(),
            )),
            "0" | "1" | "3" | "5" | "7" => {
                self.effects
                    .push(TerminalSideEffect::UnsupportedHostCommand {
                        protocol: "conemu".to_owned(),
                        command: data.to_owned(),
                    });
            }
            _ => self
                .effects
                .push(TerminalSideEffect::ConEmuControl(data.to_owned())),
        }
    }

    fn push_iterm2_side_effect(&mut self, data: &str, actions: &mut Vec<TerminalHostAction>) {
        match data {
            "ClearScrollback" => {
                actions.push(TerminalHostAction::WriteVt(b"\x1b[3J"));
                self.push_iterm2_control(data);
            }
            "SetMark" => self.effects.push(TerminalSideEffect::SemanticPrompt(
                "iterm2-set-mark".to_owned(),
            )),
            "StealFocus" => self.effects.push(TerminalSideEffect::FocusWindow),
            "ReportCellSize" => self.effects.push(TerminalSideEffect::ReportCellSize),
            "EndCopy" => {
                if let Some(capture) = self.iterm_copy_capture.take() {
                    self.effects.push(TerminalSideEffect::ClipboardWrite(
                        String::from_utf8_lossy(&capture.text).into_owned(),
                    ));
                }
            }
            _ => self.push_iterm2_assignment_side_effect(data, actions),
        }
    }

    fn push_iterm2_assignment_side_effect(
        &mut self,
        data: &str,
        actions: &mut Vec<TerminalHostAction>,
    ) {
        let Some((key, value)) = data.split_once('=') else {
            self.push_iterm2_control(data);
            return;
        };
        match key {
            "CurrentDir" => self.push_iterm2_control(data),
            "CursorShape" => {
                if let Some(sequence) = iterm_cursor_shape_sequence(value) {
                    actions.push(TerminalHostAction::WriteVt(sequence));
                }
                self.push_iterm2_control(data);
            }
            "Copy" => {
                if let Ok(bytes) = general_purpose::STANDARD.decode(value) {
                    self.effects.push(TerminalSideEffect::ClipboardWrite(
                        String::from_utf8_lossy(&bytes).into_owned(),
                    ));
                }
            }
            "CopyToClipboard" => {
                self.iterm_copy_capture = Some(ItermCopyCapture::default());
                self.push_iterm2_control(data);
            }
            "OpenURL" => match general_purpose::STANDARD.decode(value) {
                Ok(bytes) => self.effects.push(TerminalSideEffect::OpenUrl(
                    String::from_utf8_lossy(&bytes).into_owned(),
                )),
                Err(_) => self
                    .effects
                    .push(TerminalSideEffect::OpenUrl(value.to_owned())),
            },
            "File" => self
                .effects
                .push(TerminalSideEffect::Iterm2File(data.to_owned())),
            "ReportVariable" => match general_purpose::STANDARD.decode(value) {
                Ok(bytes) => self.effects.push(TerminalSideEffect::ReportVariable(
                    String::from_utf8_lossy(&bytes).into_owned(),
                )),
                Err(_) => self.push_iterm2_control(data),
            },
            "SetUserVar" => {
                if let Some(ports) = iterm2_user_var_ports(value) {
                    self.effects
                        .push(TerminalSideEffect::Iterm2UserVarPorts(ports));
                } else {
                    self.push_iterm2_control(data);
                }
            }
            _ => self.push_iterm2_control(data),
        }
    }

    fn push_osc777_side_effect(&mut self, payload: &[u8]) {
        let text = String::from_utf8_lossy(payload);
        let mut parts = text.splitn(3, ';');
        if parts.next() != Some("notify") {
            return;
        }
        let title = parts.next().unwrap_or_default().to_owned();
        let body = parts.next().unwrap_or_default().to_owned();
        self.effects
            .push(TerminalSideEffect::DesktopNotification { title, body });
    }
}

fn osc52_side_effect(payload: &[u8]) -> Option<TerminalSideEffect> {
    let separator = payload.iter().position(|byte| *byte == b';')?;
    let selection = String::from_utf8_lossy(&payload[..separator]).into_owned();
    let encoded = &payload[separator + 1..];
    if encoded == b"?" {
        return Some(TerminalSideEffect::ClipboardQuery { selection });
    }
    decode_base64(encoded)
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(TerminalSideEffect::ClipboardWrite)
}

fn decode_base64(value: impl AsRef<[u8]>) -> Option<Vec<u8>> {
    let value = value.as_ref();
    general_purpose::STANDARD
        .decode(value)
        .or_else(|_| general_purpose::STANDARD_NO_PAD.decode(value))
        .ok()
}

fn iterm2_user_var_ports(value: &str) -> Option<Vec<u16>> {
    let (name, encoded) = value.split_once('=')?;
    if name != "bootty_ports" {
        return None;
    }
    let bytes = decode_base64(encoded)?;
    let csv = std::str::from_utf8(&bytes).ok()?;
    if csv.is_empty() {
        return Some(Vec::new());
    }
    csv.split(',')
        .map(|port| port.trim().parse::<u16>())
        .collect::<Result<_, _>>()
        .ok()
}

fn is_conemu_osc9(data: &str) -> bool {
    let kind = data.split(';').next().unwrap_or_default();
    !kind.is_empty() && kind.bytes().all(|byte| byte.is_ascii_digit())
}

fn conemu_progress_state(state: &str) -> &'static str {
    match state {
        "0" | "" => "inactive",
        "1" => "normal",
        "2" => "error",
        "3" => "indeterminate",
        "4" => "warning",
        _ => "unknown",
    }
}

fn iterm_cursor_shape_sequence(shape: &str) -> Option<&'static [u8]> {
    match shape {
        "0" => Some(b"\x1b[2 q"),
        "1" => Some(b"\x1b[6 q"),
        "2" => Some(b"\x1b[4 q"),
        _ => None,
    }
}

impl ItermCopyCapture {
    fn append_plain_text(&mut self, data: &[u8]) {
        for &byte in data {
            self.escape = match self.escape {
                ItermCopyEscape::None => match byte {
                    0x1b => ItermCopyEscape::Escape,
                    b'\r' => {
                        self.text.push(b'\n');
                        ItermCopyEscape::None
                    }
                    byte if byte >= 0x20 || byte == b'\n' || byte == b'\t' => {
                        self.text.push(byte);
                        ItermCopyEscape::None
                    }
                    _ => ItermCopyEscape::None,
                },
                ItermCopyEscape::Escape => match byte {
                    b'[' => ItermCopyEscape::Csi,
                    b']' | b'P' | b'_' | b'^' => ItermCopyEscape::String,
                    0x20..=0x2f => ItermCopyEscape::Intermediate,
                    _ => ItermCopyEscape::None,
                },
                ItermCopyEscape::Intermediate if (0x20..=0x2f).contains(&byte) => {
                    ItermCopyEscape::Intermediate
                }
                ItermCopyEscape::Csi if !(0x40..=0x7e).contains(&byte) => ItermCopyEscape::Csi,
                ItermCopyEscape::String => match byte {
                    0x07 => ItermCopyEscape::None,
                    0x1b => ItermCopyEscape::StringEscape,
                    _ => ItermCopyEscape::String,
                },
                ItermCopyEscape::StringEscape => match byte {
                    b'\\' | 0x07 => ItermCopyEscape::None,
                    0x1b => ItermCopyEscape::StringEscape,
                    _ => ItermCopyEscape::String,
                },
                _ => ItermCopyEscape::None,
            };
        }
    }
}
