use anyhow::Result;
use eframe::egui;

use crate::{
    config::InputConfig,
    input::terminal_key,
    input_binding::{
        BindingAction, BindingElement, BindingKey, BindingMods, BindingTrigger, PaneDirection,
        parse_binding_elements,
    },
    mux::command::MuxDirection,
    terminal::{KeyInput, TerminalKey},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppAction {
    ReloadConfig,
    Ignore,
    NewWindow,
    NewMuxSession,
    Close,
    ToggleFullscreen,
    ToggleSidebarFocus,
    ToggleSidebarVisibility,
}

#[derive(Clone, Debug, PartialEq)]
pub enum KeybindAction {
    App(AppAction),
    Mux(MuxKeyAction),
    Scroll(TerminalScrollAction),
    Write(Vec<u8>),
    Font(FontSizeAction),
    CopyToClipboard,
    PasteFromClipboard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MuxKeyAction {
    NewTab,
    NextTab,
    PreviousTab,
    LastTab,
    SelectTab(u32),
    MoveTab(i32),
    SplitPane,
    SelectPane(MuxDirection),
    NextPane,
    KillPane,
    TogglePaneZoom,
    NextSession,
    PreviousSession,
    LastSession,
    SelectSession(u32),
    MoveSession(i32),
    DitchSession,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalScrollAction {
    Top,
    Bottom,
    PageUp,
    PageDown,
    Lines(i16),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FontSizeAction {
    Increase(f32),
    Decrease(f32),
    Reset,
    Set(f32),
}

#[derive(Clone, Debug)]
struct AppKeyBinding {
    leader: Option<BindingTrigger>,
    trigger: BindingTrigger,
    action: KeybindAction,
}

#[derive(Clone, Debug, Default)]
pub struct AppKeyBindings {
    bindings: Vec<AppKeyBinding>,
    leaders: Vec<BindingTrigger>,
    active_leader: Option<BindingTrigger>,
}

impl AppKeyBindings {
    pub fn from_config(input: &InputConfig) -> Result<Self> {
        Self::from_keybinds(&input.keybind)
    }

    pub fn from_keybinds(keybinds: &[String]) -> Result<Self> {
        let mut bindings = Vec::new();
        let mut leaders = Vec::new();
        for entry in keybinds {
            let elements = parse_binding_elements(entry)
                .map_err(|error| anyhow::anyhow!("invalid keybind {entry:?}: {error:?}"))?;
            let mut pending_leader = None;
            for element in elements {
                match element {
                    BindingElement::Leader(trigger) => {
                        if !leaders.contains(&trigger) {
                            leaders.push(trigger.clone());
                        }
                        pending_leader = Some(trigger);
                    }
                    BindingElement::Binding(binding) => {
                        let action = keybind_action(binding.action).map_err(|error| {
                            anyhow::anyhow!("unsupported keybind {entry:?}: {error}")
                        })?;
                        bindings.push(AppKeyBinding {
                            leader: pending_leader.take(),
                            trigger: binding.trigger,
                            action,
                        });
                    }
                    BindingElement::Chain(_) => {
                        anyhow::bail!(
                            "chain keybinds are not supported for app-level keybind actions"
                        );
                    }
                }
            }
        }
        Ok(Self {
            bindings,
            leaders,
            active_leader: None,
        })
    }

    pub fn action_for_key(
        &mut self,
        key: egui::Key,
        modifiers: egui::Modifiers,
    ) -> Option<KeybindAction> {
        self.action_for_candidates(binding_triggers_for_egui_key(key, modifiers))
    }

    pub fn action_for_input(&mut self, input: KeyInput) -> Option<KeybindAction> {
        self.action_for_candidates(binding_triggers_for_key_input(input))
    }

    fn action_for_candidates(&mut self, candidates: Vec<BindingTrigger>) -> Option<KeybindAction> {
        if let Some(leader) = self.active_leader.take() {
            return self
                .bindings
                .iter()
                .find(|binding| {
                    binding.leader.as_ref() == Some(&leader)
                        && candidates
                            .iter()
                            .any(|candidate| candidate == &binding.trigger)
                })
                .map(|binding| binding.action.clone())
                .or(Some(KeybindAction::App(AppAction::Ignore)));
        }

        if let Some(leader) = self
            .leaders
            .iter()
            .find(|leader| candidates.iter().any(|candidate| candidate == *leader))
        {
            self.active_leader = Some(leader.clone());
            return Some(KeybindAction::App(AppAction::Ignore));
        }

        self.bindings.iter().find_map(|binding| {
            (binding.leader.is_none()
                && candidates
                    .iter()
                    .any(|candidate| candidate == &binding.trigger))
            .then(|| binding.action.clone())
        })
    }
}
pub fn split_app_actions_for_bindings(
    app_key_bindings: &mut AppKeyBindings,
    events: Vec<egui::Event>,
) -> (Vec<egui::Event>, Vec<KeybindAction>) {
    let mut terminal_events = Vec::with_capacity(events.len());
    let mut actions = Vec::new();
    let mut suppress_next_text = false;
    for event in events {
        if suppress_next_text && matches!(event, egui::Event::Text(_)) {
            continue;
        }
        if matches!(event, egui::Event::Key { pressed: false, .. }) {
            suppress_next_text = false;
        }

        let action = match &event {
            egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } => app_key_bindings
                .action_for_key(*key, *modifiers)
                .or_else(|| builtin_app_action_for_key(*key, *modifiers)),
            _ => None,
        };
        if let Some(action) = action {
            if matches!(event, egui::Event::Key { .. }) {
                suppress_next_text = true;
            }
            actions.push(action);
        } else {
            terminal_events.push(event);
        }
    }
    (terminal_events, actions)
}

pub fn builtin_app_action_for_key(
    key: egui::Key,
    modifiers: egui::Modifiers,
) -> Option<KeybindAction> {
    (key == egui::Key::N
        && (modifiers.command || modifiers.mac_cmd)
        && !modifiers.alt
        && !modifiers.ctrl
        && !modifiers.shift)
        .then_some(KeybindAction::App(AppAction::NewMuxSession))
}

pub fn builtin_app_action_for_direct_key(input: KeyInput) -> Option<KeybindAction> {
    (input.key == TerminalKey::N
        && input.mods.command
        && !input.mods.alt
        && !input.mods.ctrl
        && !input.mods.shift)
        .then_some(KeybindAction::App(AppAction::NewMuxSession))
}

fn keybind_action(action: BindingAction) -> Result<KeybindAction> {
    match action {
        BindingAction::ReloadConfig => Ok(KeybindAction::App(AppAction::ReloadConfig)),
        BindingAction::Ignore => Ok(KeybindAction::App(AppAction::Ignore)),
        BindingAction::NewWindow => Ok(KeybindAction::App(AppAction::NewWindow)),
        BindingAction::NewMuxSession => Ok(KeybindAction::App(AppAction::NewMuxSession)),
        BindingAction::CloseWindow | BindingAction::CloseSurface | BindingAction::Quit => {
            Ok(KeybindAction::App(AppAction::Close))
        }
        BindingAction::ToggleFullscreen => Ok(KeybindAction::App(AppAction::ToggleFullscreen)),
        BindingAction::ToggleSidebarFocus => Ok(KeybindAction::App(AppAction::ToggleSidebarFocus)),
        BindingAction::ToggleSidebarVisibility => {
            Ok(KeybindAction::App(AppAction::ToggleSidebarVisibility))
        }
        BindingAction::NewTab => Ok(KeybindAction::Mux(MuxKeyAction::NewTab)),
        BindingAction::NextTab => Ok(KeybindAction::Mux(MuxKeyAction::NextTab)),
        BindingAction::PreviousTab => Ok(KeybindAction::Mux(MuxKeyAction::PreviousTab)),
        BindingAction::LastTab => Ok(KeybindAction::Mux(MuxKeyAction::LastTab)),
        BindingAction::SelectTab(index) => Ok(KeybindAction::Mux(MuxKeyAction::SelectTab(index))),
        BindingAction::MoveTab(delta) => Ok(KeybindAction::Mux(MuxKeyAction::MoveTab(delta))),
        BindingAction::SplitRight | BindingAction::SplitDown => {
            Ok(KeybindAction::Mux(MuxKeyAction::SplitPane))
        }
        BindingAction::SelectPane(direction) => Ok(KeybindAction::Mux(MuxKeyAction::SelectPane(
            mux_direction(direction),
        ))),
        BindingAction::NextPane => Ok(KeybindAction::Mux(MuxKeyAction::NextPane)),
        BindingAction::KillPane => Ok(KeybindAction::Mux(MuxKeyAction::KillPane)),
        BindingAction::TogglePaneZoom => Ok(KeybindAction::Mux(MuxKeyAction::TogglePaneZoom)),
        BindingAction::NextSession => Ok(KeybindAction::Mux(MuxKeyAction::NextSession)),
        BindingAction::PreviousSession => Ok(KeybindAction::Mux(MuxKeyAction::PreviousSession)),
        BindingAction::LastSession => Ok(KeybindAction::Mux(MuxKeyAction::LastSession)),
        BindingAction::SelectSession(index) => {
            Ok(KeybindAction::Mux(MuxKeyAction::SelectSession(index)))
        }
        BindingAction::MoveSession(delta) => {
            Ok(KeybindAction::Mux(MuxKeyAction::MoveSession(delta)))
        }
        BindingAction::DitchSession => Ok(KeybindAction::Mux(MuxKeyAction::DitchSession)),
        BindingAction::ScrollToTop => Ok(KeybindAction::Scroll(TerminalScrollAction::Top)),
        BindingAction::ScrollToBottom => Ok(KeybindAction::Scroll(TerminalScrollAction::Bottom)),
        BindingAction::ScrollPageUp => Ok(KeybindAction::Scroll(TerminalScrollAction::PageUp)),
        BindingAction::ScrollPageDown => Ok(KeybindAction::Scroll(TerminalScrollAction::PageDown)),
        BindingAction::ScrollPageLines(lines) => {
            Ok(KeybindAction::Scroll(TerminalScrollAction::Lines(lines)))
        }
        BindingAction::Csi(value) => Ok(KeybindAction::Write(csi_bytes(&value))),
        BindingAction::Esc(value) => Ok(KeybindAction::Write(esc_bytes(&value))),
        BindingAction::Text(value) => Ok(KeybindAction::Write(text_action_bytes(&value))),
        BindingAction::IncreaseFontSize(delta) => {
            Ok(KeybindAction::Font(FontSizeAction::Increase(delta)))
        }
        BindingAction::DecreaseFontSize(delta) => {
            Ok(KeybindAction::Font(FontSizeAction::Decrease(delta)))
        }
        BindingAction::ResetFontSize => Ok(KeybindAction::Font(FontSizeAction::Reset)),
        BindingAction::SetFontSize(size) => Ok(KeybindAction::Font(FontSizeAction::Set(size))),
        BindingAction::CopyToClipboard(_) => Ok(KeybindAction::CopyToClipboard),
        BindingAction::PasteFromClipboard => Ok(KeybindAction::PasteFromClipboard),
        unsupported => anyhow::bail!("{} has no Bootty app behavior", unsupported.format_entry()),
    }
}

fn mux_direction(direction: PaneDirection) -> MuxDirection {
    match direction {
        PaneDirection::Left => MuxDirection::Left,
        PaneDirection::Down => MuxDirection::Down,
        PaneDirection::Up => MuxDirection::Up,
        PaneDirection::Right => MuxDirection::Right,
    }
}

fn csi_bytes(value: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(value.len() + 2);
    bytes.extend_from_slice(b"\x1b[");
    bytes.extend_from_slice(value.as_bytes());
    bytes
}

fn esc_bytes(value: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(value.len() + 1);
    bytes.push(0x1b);
    bytes.extend_from_slice(value.as_bytes());
    bytes
}

fn text_action_bytes(value: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            let mut buf = [0; 4];
            bytes.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.peek().copied() {
            Some('n') => {
                chars.next();
                bytes.push(b'\n');
            }
            Some('r') => {
                chars.next();
                bytes.push(b'\r');
            }
            Some('t') => {
                chars.next();
                bytes.push(b'\t');
            }
            Some('e') => {
                chars.next();
                bytes.push(0x1b);
            }
            Some('\\') => {
                chars.next();
                bytes.push(b'\\');
            }
            Some('x') => {
                chars.next();
                let Some(high) = chars.next().and_then(|value| value.to_digit(16)) else {
                    bytes.extend_from_slice(b"\\x");
                    continue;
                };
                let Some(low) = chars.next().and_then(|value| value.to_digit(16)) else {
                    bytes.extend_from_slice(format!("\\x{high:x}").as_bytes());
                    continue;
                };
                bytes.push(((high << 4) | low) as u8);
            }
            Some(other) => {
                chars.next();
                bytes.push(b'\\');
                let mut buf = [0; 4];
                bytes.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
            None => bytes.push(b'\\'),
        }
    }
    bytes
}

fn binding_triggers_for_egui_key(
    key: egui::Key,
    modifiers: egui::Modifiers,
) -> Vec<BindingTrigger> {
    let mods = BindingMods {
        shift: modifiers.shift,
        ctrl: modifiers.ctrl,
        alt: modifiers.alt,
        command: modifiers.command || modifiers.mac_cmd,
    };
    let Some(terminal_key) = terminal_key(key) else {
        return Vec::new();
    };
    let mut triggers = vec![BindingTrigger {
        mods,
        key: BindingKey::Physical(terminal_key),
    }];
    if let Some(ch) = binding_char_for_egui_key(key) {
        triggers.push(BindingTrigger {
            mods,
            key: BindingKey::Unicode(ch),
        });
    }
    triggers
}

fn binding_triggers_for_key_input(input: KeyInput) -> Vec<BindingTrigger> {
    let mods = BindingMods::from(input.mods);
    let mut triggers = vec![BindingTrigger {
        mods,
        key: BindingKey::Physical(input.key),
    }];
    if let Some(ch) = input.unshifted.or_else(|| input.utf8.and_then(single_char)) {
        triggers.push(BindingTrigger {
            mods,
            key: BindingKey::Unicode(ch),
        });
    }
    triggers
}

fn single_char(value: &str) -> Option<char> {
    let mut chars = value.chars();
    let ch = chars.next()?;
    chars.next().is_none().then_some(ch)
}

fn binding_char_for_egui_key(key: egui::Key) -> Option<char> {
    Some(match key {
        egui::Key::A => 'a',
        egui::Key::B => 'b',
        egui::Key::C => 'c',
        egui::Key::D => 'd',
        egui::Key::E => 'e',
        egui::Key::F => 'f',
        egui::Key::G => 'g',
        egui::Key::H => 'h',
        egui::Key::I => 'i',
        egui::Key::J => 'j',
        egui::Key::K => 'k',
        egui::Key::L => 'l',
        egui::Key::M => 'm',
        egui::Key::N => 'n',
        egui::Key::O => 'o',
        egui::Key::P => 'p',
        egui::Key::Q => 'q',
        egui::Key::R => 'r',
        egui::Key::S => 's',
        egui::Key::T => 't',
        egui::Key::U => 'u',
        egui::Key::V => 'v',
        egui::Key::W => 'w',
        egui::Key::X => 'x',
        egui::Key::Y => 'y',
        egui::Key::Z => 'z',
        egui::Key::Num0 => '0',
        egui::Key::Num1 => '1',
        egui::Key::Num2 => '2',
        egui::Key::Num3 => '3',
        egui::Key::Num4 => '4',
        egui::Key::Num5 => '5',
        egui::Key::Num6 => '6',
        egui::Key::Num7 => '7',
        egui::Key::Num8 => '8',
        egui::Key::Num9 => '9',
        egui::Key::Comma => ',',
        egui::Key::Period => '.',
        egui::Key::Slash => '/',
        egui::Key::Semicolon => ';',
        egui::Key::Quote => '\'',
        egui::Key::Minus => '-',
        egui::Key::Plus | egui::Key::Equals => '=',
        egui::Key::Backslash => '\\',
        egui::Key::Backtick => '`',
        egui::Key::Space => ' ',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_keybindings_route_default_reload_shortcut() {
        let mut bindings = AppKeyBindings::from_config(&InputConfig::default()).unwrap();
        let action = bindings.action_for_key(
            egui::Key::R,
            egui::Modifiers {
                shift: true,
                command: true,
                ..Default::default()
            },
        );

        assert_eq!(action, Some(KeybindAction::App(AppAction::ReloadConfig)));
    }

    #[test]
    fn builtin_cmd_n_creates_mux_session_not_terminal_input() {
        let action = builtin_app_action_for_key(
            egui::Key::N,
            egui::Modifiers {
                command: true,
                ..Default::default()
            },
        );

        assert_eq!(action, Some(KeybindAction::App(AppAction::NewMuxSession)));
    }

    #[test]
    fn builtin_macos_cmd_n_creates_mux_session_not_terminal_input() {
        let action = builtin_app_action_for_key(
            egui::Key::N,
            egui::Modifiers {
                mac_cmd: true,
                ..Default::default()
            },
        );

        assert_eq!(action, Some(KeybindAction::App(AppAction::NewMuxSession)));
    }

    #[test]
    fn builtin_direct_cmd_n_creates_mux_session_not_terminal_input() {
        let action = builtin_app_action_for_direct_key(KeyInput {
            key: TerminalKey::N,
            mods: crate::terminal::KeyMods {
                command: true,
                ..Default::default()
            },
            repeat: false,
            utf8: Some("n"),
            unshifted: Some('n'),
        });

        assert_eq!(action, Some(KeybindAction::App(AppAction::NewMuxSession)));
    }

    #[test]
    fn split_app_actions_consumes_text_paired_with_macos_cmd_n() {
        let mut bindings = AppKeyBindings::default();
        let (terminal_events, actions) = split_app_actions_for_bindings(
            &mut bindings,
            vec![
                egui::Event::Key {
                    key: egui::Key::N,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::MAC_CMD,
                },
                egui::Event::Text("~2".to_owned()),
                egui::Event::Text("2~".to_owned()),
                egui::Event::Key {
                    key: egui::Key::N,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers: egui::Modifiers::MAC_CMD,
                },
            ],
        );

        assert!(
            !terminal_events
                .iter()
                .any(|event| matches!(event, egui::Event::Text(_)))
        );
        assert_eq!(actions, vec![KeybindAction::App(AppAction::NewMuxSession)]);
    }

    #[test]
    fn split_app_actions_consumes_text_paired_with_leader_sequence_binding() {
        let mut bindings = AppKeyBindings::from_config(&InputConfig {
            keybind: vec!["ctrl+space>c=new_tab".to_owned()],
            ..Default::default()
        })
        .unwrap();
        let (terminal_events, actions) = split_app_actions_for_bindings(
            &mut bindings,
            vec![
                egui::Event::Key {
                    key: egui::Key::Space,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers {
                        ctrl: true,
                        ..Default::default()
                    },
                },
                egui::Event::Key {
                    key: egui::Key::C,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::default(),
                },
                egui::Event::Text("c".to_owned()),
            ],
        );

        assert!(
            !terminal_events
                .iter()
                .any(|event| matches!(event, egui::Event::Text(_)))
        );
        assert_eq!(
            actions,
            vec![
                KeybindAction::App(AppAction::Ignore),
                KeybindAction::Mux(MuxKeyAction::NewTab)
            ]
        );
    }

    #[test]
    fn app_keybindings_route_escape_and_text_actions_to_terminal_bytes() {
        let mut bindings = AppKeyBindings::from_config(&InputConfig {
            keybind: vec![
                "cmd+b=esc:090;8~".to_owned(),
                "shift+Enter=text:\\n".to_owned(),
                "cmd+1=text:\\x001".to_owned(),
                "cmd+o=ignore".to_owned(),
                "cmd+==increase_font_size:1".to_owned(),
                "cmd+0=reset_font_size".to_owned(),
                "performable:cmd+c=copy_to_clipboard".to_owned(),
                "performable:cmd+v=paste_from_clipboard".to_owned(),
                "cmd+alt+n=new_window".to_owned(),
                "cmd+q=quit".to_owned(),
                "cmd+alt+ctrl+f=toggle_fullscreen".to_owned(),
            ],
            ..Default::default()
        })
        .unwrap();

        let cmd = egui::Modifiers {
            command: true,
            ..Default::default()
        };
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };

        assert_eq!(
            bindings.action_for_key(egui::Key::B, cmd),
            Some(KeybindAction::Write(b"\x1b090;8~".to_vec()))
        );
        assert_eq!(
            bindings.action_for_input(KeyInput {
                key: TerminalKey::B,
                mods: crate::terminal::KeyMods {
                    command: true,
                    ..Default::default()
                },
                repeat: false,
                utf8: Some("b"),
                unshifted: Some('b'),
            }),
            Some(KeybindAction::Write(b"\x1b090;8~".to_vec()))
        );
        assert_eq!(
            bindings.action_for_key(egui::Key::Enter, shift),
            Some(KeybindAction::Write(b"\n".to_vec()))
        );
        assert_eq!(
            bindings.action_for_key(egui::Key::Num1, cmd),
            Some(KeybindAction::Write(vec![0x00, b'1']))
        );
        assert_eq!(
            bindings.action_for_key(egui::Key::O, cmd),
            Some(KeybindAction::App(AppAction::Ignore))
        );
        assert_eq!(
            bindings.action_for_key(egui::Key::Equals, cmd),
            Some(KeybindAction::Font(FontSizeAction::Increase(1.0)))
        );
        assert_eq!(
            bindings.action_for_key(egui::Key::Num0, cmd),
            Some(KeybindAction::Font(FontSizeAction::Reset))
        );
        assert_eq!(
            bindings.action_for_key(egui::Key::C, cmd),
            Some(KeybindAction::CopyToClipboard)
        );
        assert_eq!(
            bindings.action_for_key(egui::Key::V, cmd),
            Some(KeybindAction::PasteFromClipboard)
        );
        assert_eq!(
            bindings.action_for_key(
                egui::Key::N,
                egui::Modifiers {
                    command: true,
                    alt: true,
                    ..Default::default()
                }
            ),
            Some(KeybindAction::App(AppAction::NewWindow))
        );
        assert_eq!(
            bindings.action_for_key(egui::Key::Q, cmd),
            Some(KeybindAction::App(AppAction::Close))
        );
        assert_eq!(
            bindings.action_for_key(
                egui::Key::F,
                egui::Modifiers {
                    command: true,
                    alt: true,
                    ctrl: true,
                    ..Default::default()
                }
            ),
            Some(KeybindAction::App(AppAction::ToggleFullscreen))
        );
    }

    #[test]
    fn app_keybindings_reject_unsupported_configured_actions() {
        let error = AppKeyBindings::from_config(&InputConfig {
            keybind: vec!["cmd+a=select_all".to_owned()],
            ..Default::default()
        })
        .unwrap_err()
        .to_string();

        assert!(error.contains("unsupported keybind"));
        assert!(error.contains("select_all"));
    }
}
