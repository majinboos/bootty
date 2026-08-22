use std::sync::{Arc, Mutex, mpsc::Sender};

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
    callback_effects: Arc<Mutex<Vec<TerminalSideEffect>>>,
    iterm_copy_capture: Option<Vec<u8>>,
}

impl TerminalSideEffectCollector {
    pub(crate) fn new() -> Self {
        Self {
            osc_pending: Vec::new(),
            effects: Vec::new(),
            callback_effects: Arc::new(Mutex::new(Vec::new())),
            iterm_copy_capture: None,
        }
    }

    pub(crate) fn callback_effects(&self) -> Arc<Mutex<Vec<TerminalSideEffect>>> {
        self.callback_effects.clone()
    }

    pub(crate) fn has_pending_osc(&self) -> bool {
        !self.osc_pending.is_empty()
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
        if let Ok(mut effects) = self.callback_effects.lock() {
            self.effects.extend(effects.drain(..));
        }
        std::mem::take(&mut self.effects)
    }

    fn push_osc_side_effect(&mut self, payload: &[u8], actions: &mut Vec<TerminalHostAction>) {
        let Some((command, rest)) = split_osc_payload(payload) else {
            return;
        };
        match command {
            b"1" => {
                if let Ok(icon) = std::str::from_utf8(rest) {
                    self.effects
                        .push(TerminalSideEffect::WindowIcon(icon.to_owned()));
                }
            }
            b"9" => {
                if let Ok(data) = std::str::from_utf8(rest) {
                    if conemu_osc9_kind(data).is_some() {
                        self.push_conemu_side_effect(data);
                    } else {
                        self.effects.push(TerminalSideEffect::DesktopNotification {
                            title: String::new(),
                            body: data.to_owned(),
                        });
                    }
                }
            }
            b"22" => {
                if let Ok(shape) = std::str::from_utf8(rest) {
                    self.effects
                        .push(TerminalSideEffect::MouseShape(shape.to_owned()));
                }
            }
            b"52" => match osc52_payload_text(rest) {
                Some(Ok(text)) => self.effects.push(TerminalSideEffect::ClipboardWrite(text)),
                Some(Err(selection)) => self
                    .effects
                    .push(TerminalSideEffect::ClipboardQuery { selection }),
                None => {}
            },
            b"133" => {
                if let Ok(data) = std::str::from_utf8(rest) {
                    self.effects
                        .push(TerminalSideEffect::SemanticPrompt(data.to_owned()));
                }
            }
            b"66" => {
                if let Ok(data) = std::str::from_utf8(rest) {
                    self.effects
                        .push(TerminalSideEffect::KittyTextSizing(data.to_owned()));
                }
            }
            b"777" => self.push_osc777_side_effect(rest),
            b"1337" => {
                if let Ok(data) = std::str::from_utf8(rest) {
                    self.push_iterm2_side_effect(data, actions);
                }
            }
            _ => {}
        }
    }

    fn append_iterm_copy_text(&mut self, data: &[u8]) {
        let Some(capture) = self.iterm_copy_capture.as_mut() else {
            return;
        };
        append_plain_text_bytes(capture, data);
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
                self.effects
                    .push(TerminalSideEffect::Iterm2Control(data.to_owned()));
            }
            "SetMark" => self.effects.push(TerminalSideEffect::SemanticPrompt(
                "iterm2-set-mark".to_owned(),
            )),
            "StealFocus" => self.effects.push(TerminalSideEffect::FocusWindow),
            "ReportCellSize" => self.effects.push(TerminalSideEffect::ReportCellSize),
            "EndCopy" => {
                if let Some(capture) = self.iterm_copy_capture.take() {
                    self.effects.push(TerminalSideEffect::ClipboardWrite(
                        String::from_utf8_lossy(&capture).into_owned(),
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
            self.effects
                .push(TerminalSideEffect::Iterm2Control(data.to_owned()));
            return;
        };
        match key {
            "CurrentDir" => self
                .effects
                .push(TerminalSideEffect::Iterm2Control(data.to_owned())),
            "CursorShape" => {
                if let Some(sequence) = iterm_cursor_shape_sequence(value) {
                    actions.push(TerminalHostAction::WriteVt(sequence));
                }
                self.effects
                    .push(TerminalSideEffect::Iterm2Control(data.to_owned()));
            }
            "Copy" => {
                if let Ok(bytes) = general_purpose::STANDARD.decode(value) {
                    self.effects.push(TerminalSideEffect::ClipboardWrite(
                        String::from_utf8_lossy(&bytes).into_owned(),
                    ));
                }
            }
            "CopyToClipboard" => {
                self.iterm_copy_capture = Some(Vec::new());
                self.effects
                    .push(TerminalSideEffect::Iterm2Control(data.to_owned()));
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
                Err(_) => self
                    .effects
                    .push(TerminalSideEffect::Iterm2Control(data.to_owned())),
            },
            "SetUserVar" => {
                if let Some(ports) = iterm2_user_var_ports(value) {
                    self.effects
                        .push(TerminalSideEffect::Iterm2UserVarPorts(ports));
                } else {
                    self.effects
                        .push(TerminalSideEffect::Iterm2Control(data.to_owned()));
                }
            }
            _ => self
                .effects
                .push(TerminalSideEffect::Iterm2Control(data.to_owned())),
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

fn osc52_payload_text(payload: &[u8]) -> Option<Result<String, String>> {
    let separator = payload.iter().position(|byte| *byte == b';')?;
    let selection = String::from_utf8_lossy(&payload[..separator]).into_owned();
    let encoded = &payload[separator + 1..];
    if encoded == b"?" {
        return Some(Err(selection));
    }
    let bytes = general_purpose::STANDARD
        .decode(encoded)
        .or_else(|_| general_purpose::STANDARD_NO_PAD.decode(encoded))
        .ok()?;
    String::from_utf8(bytes).ok().map(Ok)
}

fn iterm2_user_var_ports(value: &str) -> Option<Vec<u16>> {
    let (name, encoded) = value.split_once('=')?;
    if name != "bootty_ports" {
        return None;
    }
    let bytes = general_purpose::STANDARD
        .decode(encoded)
        .or_else(|_| general_purpose::STANDARD_NO_PAD.decode(encoded))
        .ok()?;
    let csv = std::str::from_utf8(&bytes).ok()?;
    if csv.is_empty() {
        return Some(Vec::new());
    }
    csv.split(',')
        .map(|port| port.trim().parse::<u16>())
        .collect::<Result<_, _>>()
        .ok()
}

fn conemu_osc9_kind(data: &str) -> Option<&str> {
    let kind = data.split(';').next().unwrap_or_default();
    (!kind.is_empty() && kind.bytes().all(|byte| byte.is_ascii_digit())).then_some(kind)
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

fn append_plain_text_bytes(out: &mut Vec<u8>, data: &[u8]) {
    let mut index = 0;
    while index < data.len() {
        match data[index] {
            0x1b => index = skip_escape_sequence(data, index),
            b'\r' => {
                out.push(b'\n');
                index += 1;
            }
            byte if byte >= 0x20 || byte == b'\n' || byte == b'\t' => {
                out.push(byte);
                index += 1;
            }
            _ => index += 1,
        }
    }
}

fn skip_escape_sequence(data: &[u8], start: usize) -> usize {
    match data.get(start + 1).copied() {
        Some(b'[') => data[start + 2..]
            .iter()
            .position(|byte| (0x40..=0x7e).contains(byte))
            .map_or(data.len(), |end| start + 3 + end),
        Some(b']' | b'P' | b'_') => find_osc_terminator(&data[start + 2..])
            .map_or(data.len(), |(len, term)| start + 2 + len + term),
        Some(_) => (start + 2).min(data.len()),
        None => data.len(),
    }
}
