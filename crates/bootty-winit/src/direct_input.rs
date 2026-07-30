use std::borrow::Borrow;

use eframe::egui;
use winit::{
    event::{ElementState, KeyEvent},
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
};

use crate::{
    input_keymap::bare_terminal_key_input, modifier_remap::ModifierRemapSet, terminal::KeyInput,
};

pub use crate::input_keymap::ModifierSideState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectKeyInput {
    pub input: KeyInput,
    suppress_egui_key: Option<egui::Key>,
}

impl DirectKeyInput {
    pub fn input(self) -> KeyInput {
        self.input
    }
}

pub fn direct_key_input_from_winit_event(
    event: &KeyEvent,
    modifiers: ModifiersState,
    side_state: ModifierSideState,
) -> Option<DirectKeyInput> {
    if event.state != ElementState::Pressed {
        return None;
    }
    let PhysicalKey::Code(code) = event.physical_key else {
        return None;
    };
    direct_key_input_from_winit_code(code, modifiers, side_state, event.repeat)
}

pub fn direct_key_input_from_winit_code(
    code: KeyCode,
    modifiers: ModifiersState,
    side_state: ModifierSideState,
    repeat: bool,
) -> Option<DirectKeyInput> {
    direct_key_input_from_winit_code_with_remaps(
        code,
        modifiers,
        side_state,
        repeat,
        &ModifierRemapSet::default(),
    )
}

pub fn direct_key_input_from_winit_code_with_remaps(
    code: KeyCode,
    modifiers: ModifiersState,
    side_state: ModifierSideState,
    repeat: bool,
    modifier_remaps: &ModifierRemapSet,
) -> Option<DirectKeyInput> {
    let suppress_egui_key = collapsed_egui_key_for_direct_code(code)
        .or_else(|| side_sensitive_egui_key_for_direct_code(code, modifiers, side_state))
        .or_else(|| command_egui_key_for_direct_code(code, modifiers, side_state))
        .or_else(|| linux_terminal_shortcut_egui_key_for_direct_code(code, modifiers))
        .or_else(|| windows_terminal_shortcut_egui_key_for_direct_code(code, modifiers))
        .or_else(|| modifier_egui_key_for_direct_code(code))?;
    let mut input = bare_terminal_key_input(code, modifiers, repeat)?;
    side_state.apply_to_key_input(&mut input);
    input.mods = modifier_remaps.apply(input.mods);
    Some(DirectKeyInput {
        input,
        suppress_egui_key,
    })
}

pub fn suppress_egui_events_for_direct_input(
    events: &mut Vec<egui::Event>,
    direct_inputs: &[DirectKeyInput],
) {
    if events.is_empty() || direct_inputs.is_empty() {
        return;
    }

    let mut key_counts = Vec::new();
    let mut text_counts = Vec::new();
    for direct_input in direct_inputs {
        if let Some(key) = direct_input.suppress_egui_key {
            increment_count(&mut key_counts, key);
        }
        if let Some(utf8) = direct_input.input.utf8 {
            increment_count(&mut text_counts, utf8);
        }
    }

    events.retain(|event| match event {
        egui::Event::Key {
            key,
            physical_key,
            pressed: true,
            ..
        } if (physical_key.is_none() || *physical_key == Some(*key))
            && take_count(&mut key_counts, key) =>
        {
            false
        }
        egui::Event::Text(text) if take_count(&mut text_counts, text.as_str()) => false,
        _ => true,
    });
}

fn collapsed_egui_key_for_direct_code(code: KeyCode) -> Option<Option<egui::Key>> {
    let key = match code {
        KeyCode::Numpad0 => egui::Key::Num0,
        KeyCode::Numpad1 => egui::Key::Num1,
        KeyCode::Numpad2 => egui::Key::Num2,
        KeyCode::Numpad3 => egui::Key::Num3,
        KeyCode::Numpad4 => egui::Key::Num4,
        KeyCode::Numpad5 => egui::Key::Num5,
        KeyCode::Numpad6 => egui::Key::Num6,
        KeyCode::Numpad7 => egui::Key::Num7,
        KeyCode::Numpad8 => egui::Key::Num8,
        KeyCode::Numpad9 => egui::Key::Num9,
        KeyCode::NumpadAdd => egui::Key::Plus,
        KeyCode::NumpadDivide => egui::Key::Slash,
        KeyCode::NumpadEnter => egui::Key::Enter,
        KeyCode::NumpadSubtract => egui::Key::Minus,
        KeyCode::NumpadDecimal | KeyCode::NumpadEqual | KeyCode::NumpadMultiply => {
            return Some(None);
        }
        _ => return None,
    };
    Some(Some(key))
}

fn side_sensitive_egui_key_for_direct_code(
    code: KeyCode,
    modifiers: ModifiersState,
    side_state: ModifierSideState,
) -> Option<Option<egui::Key>> {
    if code == KeyCode::Tab && modifiers.shift_key() && side_state.has_right_shift() {
        return Some(Some(egui::Key::Tab));
    }
    None
}

#[cfg(target_os = "linux")]
fn linux_terminal_shortcut_egui_key_for_direct_code(
    code: KeyCode,
    modifiers: ModifiersState,
) -> Option<Option<egui::Key>> {
    (code == KeyCode::KeyC && modifiers.control_key()).then_some(Some(egui::Key::C))
}

#[cfg(not(target_os = "linux"))]
fn linux_terminal_shortcut_egui_key_for_direct_code(
    _code: KeyCode,
    _modifiers: ModifiersState,
) -> Option<Option<egui::Key>> {
    None
}

fn command_egui_key_for_direct_code(
    code: KeyCode,
    modifiers: ModifiersState,
    side_state: ModifierSideState,
) -> Option<Option<egui::Key>> {
    (modifiers.super_key() || side_state.has_command())
        .then(|| egui_key_for_direct_code(code).map(Some))
        .flatten()
}

#[cfg(windows)]
fn windows_terminal_shortcut_egui_key_for_direct_code(
    code: KeyCode,
    modifiers: ModifiersState,
) -> Option<Option<egui::Key>> {
    if modifiers.control_key() || (code == KeyCode::Insert && modifiers.shift_key()) {
        return egui_key_for_direct_code(code).map(Some);
    }
    None
}

#[cfg(not(windows))]
fn windows_terminal_shortcut_egui_key_for_direct_code(
    _code: KeyCode,
    _modifiers: ModifiersState,
) -> Option<Option<egui::Key>> {
    None
}

fn egui_key_for_direct_code(code: KeyCode) -> Option<egui::Key> {
    Some(match code {
        KeyCode::KeyA => egui::Key::A,
        KeyCode::KeyB => egui::Key::B,
        KeyCode::KeyC => egui::Key::C,
        KeyCode::KeyD => egui::Key::D,
        KeyCode::KeyE => egui::Key::E,
        KeyCode::KeyF => egui::Key::F,
        KeyCode::KeyG => egui::Key::G,
        KeyCode::KeyH => egui::Key::H,
        KeyCode::KeyI => egui::Key::I,
        KeyCode::KeyJ => egui::Key::J,
        KeyCode::KeyK => egui::Key::K,
        KeyCode::KeyL => egui::Key::L,
        KeyCode::KeyM => egui::Key::M,
        KeyCode::KeyN => egui::Key::N,
        KeyCode::KeyO => egui::Key::O,
        KeyCode::KeyP => egui::Key::P,
        KeyCode::KeyQ => egui::Key::Q,
        KeyCode::KeyR => egui::Key::R,
        KeyCode::KeyS => egui::Key::S,
        KeyCode::KeyT => egui::Key::T,
        KeyCode::KeyU => egui::Key::U,
        KeyCode::KeyV => egui::Key::V,
        KeyCode::KeyW => egui::Key::W,
        KeyCode::KeyX => egui::Key::X,
        KeyCode::KeyY => egui::Key::Y,
        KeyCode::KeyZ => egui::Key::Z,
        KeyCode::Digit0 => egui::Key::Num0,
        KeyCode::Digit1 => egui::Key::Num1,
        KeyCode::Digit2 => egui::Key::Num2,
        KeyCode::Digit3 => egui::Key::Num3,
        KeyCode::Digit4 => egui::Key::Num4,
        KeyCode::Digit5 => egui::Key::Num5,
        KeyCode::Digit6 => egui::Key::Num6,
        KeyCode::Digit7 => egui::Key::Num7,
        KeyCode::Digit8 => egui::Key::Num8,
        KeyCode::Digit9 => egui::Key::Num9,
        KeyCode::Comma => egui::Key::Comma,
        KeyCode::Period => egui::Key::Period,
        KeyCode::Slash => egui::Key::Slash,
        KeyCode::Semicolon => egui::Key::Semicolon,
        KeyCode::Quote => egui::Key::Quote,
        KeyCode::Minus => egui::Key::Minus,
        KeyCode::Equal => egui::Key::Equals,
        KeyCode::Backslash => egui::Key::Backslash,
        KeyCode::Backquote => egui::Key::Backtick,
        KeyCode::Space => egui::Key::Space,
        KeyCode::Enter => egui::Key::Enter,
        KeyCode::Tab => egui::Key::Tab,
        KeyCode::Backspace => egui::Key::Backspace,
        KeyCode::Escape => egui::Key::Escape,
        KeyCode::ArrowUp => egui::Key::ArrowUp,
        KeyCode::ArrowDown => egui::Key::ArrowDown,
        KeyCode::ArrowRight => egui::Key::ArrowRight,
        KeyCode::ArrowLeft => egui::Key::ArrowLeft,
        KeyCode::Delete => egui::Key::Delete,
        KeyCode::Home => egui::Key::Home,
        KeyCode::End => egui::Key::End,
        KeyCode::PageUp => egui::Key::PageUp,
        KeyCode::PageDown => egui::Key::PageDown,
        KeyCode::Insert => egui::Key::Insert,
        KeyCode::F1 => egui::Key::F1,
        KeyCode::F2 => egui::Key::F2,
        KeyCode::F3 => egui::Key::F3,
        KeyCode::F4 => egui::Key::F4,
        KeyCode::F5 => egui::Key::F5,
        KeyCode::F6 => egui::Key::F6,
        KeyCode::F7 => egui::Key::F7,
        KeyCode::F8 => egui::Key::F8,
        KeyCode::F9 => egui::Key::F9,
        KeyCode::F10 => egui::Key::F10,
        KeyCode::F11 => egui::Key::F11,
        KeyCode::F12 => egui::Key::F12,
        _ => return None,
    })
}

fn modifier_egui_key_for_direct_code(code: KeyCode) -> Option<Option<egui::Key>> {
    match code {
        KeyCode::ShiftLeft
        | KeyCode::ShiftRight
        | KeyCode::ControlLeft
        | KeyCode::ControlRight
        | KeyCode::AltLeft
        | KeyCode::AltRight => Some(None),
        _ => None,
    }
}

fn increment_count<T: Eq>(counts: &mut Vec<(T, usize)>, value: T) {
    if let Some((_, count)) = counts.iter_mut().find(|(candidate, _)| *candidate == value) {
        *count += 1;
    } else {
        counts.push((value, 1));
    }
}

fn take_count<C, T>(counts: &mut [(C, usize)], value: &T) -> bool
where
    C: Borrow<T>,
    T: Eq + ?Sized,
{
    if let Some((_, count)) = counts
        .iter_mut()
        .find(|(candidate, count)| *count > 0 && candidate.borrow() == value)
    {
        *count -= 1;
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input_binding::{BindingKey, BindingMods, BindingTrigger};
    use crate::modifier_remap::ModifierRemapSet;
    use crate::terminal::{KeyMods, TerminalKey};

    #[test]
    fn direct_keypad_input_preserves_physical_identity_for_bootty_winit() {
        let direct = direct_key_input_from_winit_code(
            KeyCode::Numpad1,
            ModifiersState::ALT,
            ModifierSideState::default(),
            false,
        )
        .expect("numpad key maps to direct terminal input");

        assert_eq!(direct.input.key, TerminalKey::Numpad1);
        assert_eq!(
            direct.input.mods,
            KeyMods {
                alt: true,
                ..Default::default()
            }
        );
        assert_eq!(direct.input.utf8, Some("1"));
        assert_eq!(direct.input.unshifted, Some('1'));
    }

    #[test]
    fn direct_keypad_input_suppresses_collapsed_egui_events() {
        let direct = direct_key_input_from_winit_code(
            KeyCode::Numpad1,
            ModifiersState::empty(),
            ModifierSideState::default(),
            false,
        )
        .expect("numpad key maps to direct terminal input");
        let mut events = vec![
            egui::Event::Key {
                key: egui::Key::Num1,
                physical_key: Some(egui::Key::Num1),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            },
            egui::Event::Text("1".to_owned()),
            egui::Event::Key {
                key: egui::Key::A,
                physical_key: Some(egui::Key::A),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            },
        ];

        suppress_egui_events_for_direct_input(&mut events, &[direct]);

        assert_eq!(
            events,
            vec![egui::Event::Key {
                key: egui::Key::A,
                physical_key: Some(egui::Key::A),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            }]
        );
    }

    #[test]
    fn direct_input_suppresses_repeated_events_without_dropping_unmatched_events() {
        let direct = direct_key_input_from_winit_code(
            KeyCode::Numpad1,
            ModifiersState::empty(),
            ModifierSideState::default(),
            false,
        )
        .expect("numpad key maps to direct terminal input");
        let repeated = direct_key_input_from_winit_code(
            KeyCode::Numpad1,
            ModifiersState::empty(),
            ModifierSideState::default(),
            true,
        )
        .expect("repeat maps to direct terminal input");
        let mut events = vec![
            egui::Event::Key {
                key: egui::Key::Num1,
                physical_key: Some(egui::Key::Num1),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            },
            egui::Event::Text("1".to_owned()),
            egui::Event::PointerMoved(egui::pos2(1.0, 2.0)),
            egui::Event::Key {
                key: egui::Key::Num1,
                physical_key: Some(egui::Key::Num1),
                pressed: true,
                repeat: true,
                modifiers: egui::Modifiers::default(),
            },
            egui::Event::Text("1".to_owned()),
            egui::Event::Text("1".to_owned()),
        ];

        suppress_egui_events_for_direct_input(&mut events, &[direct, repeated]);

        assert_eq!(
            events,
            vec![
                egui::Event::PointerMoved(egui::pos2(1.0, 2.0)),
                egui::Event::Text("1".to_owned()),
            ]
        );
    }

    #[test]
    fn direct_input_leaves_main_row_digits_on_egui_path() {
        assert!(
            direct_key_input_from_winit_code(
                KeyCode::Digit1,
                ModifiersState::empty(),
                ModifierSideState::default(),
                false,
            )
            .is_none()
        );
    }

    #[test]
    fn direct_input_preserves_command_modified_regular_keys_for_configured_bindings() {
        let direct = direct_key_input_from_winit_code(
            KeyCode::KeyB,
            ModifiersState::SUPER,
            ModifierSideState::default(),
            false,
        )
        .expect("command-modified regular key maps to direct terminal input");

        assert_eq!(direct.input.key, TerminalKey::B);
        assert_eq!(
            direct.input.mods,
            KeyMods {
                command: true,
                ..Default::default()
            }
        );
        assert_eq!(direct.suppress_egui_key, Some(egui::Key::B));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn direct_input_preserves_ctrl_c_for_terminal_bindings() {
        let direct = direct_key_input_from_winit_code(
            KeyCode::KeyC,
            ModifiersState::CONTROL,
            ModifierSideState::default(),
            false,
        )
        .expect("ctrl+c maps to direct terminal input");

        assert_eq!(direct.input.key, TerminalKey::C);
        assert_eq!(
            direct.input.mods,
            KeyMods {
                ctrl: true,
                ..Default::default()
            }
        );
        assert_eq!(direct.suppress_egui_key, Some(egui::Key::C));
    }

    #[cfg(windows)]
    #[test]
    fn direct_input_preserves_control_modified_regular_keys_for_windows_bindings() {
        let direct = direct_key_input_from_winit_code(
            KeyCode::KeyV,
            ModifiersState::CONTROL,
            ModifierSideState::default(),
            false,
        )
        .expect("control-modified regular key maps to direct terminal input on Windows");

        assert_eq!(direct.input.key, TerminalKey::V);
        assert_eq!(
            direct.input.mods,
            KeyMods {
                ctrl: true,
                ..Default::default()
            }
        );
        assert_eq!(direct.suppress_egui_key, Some(egui::Key::V));
    }

    #[cfg(windows)]
    #[test]
    fn direct_input_preserves_shift_insert_for_windows_paste_binding() {
        let direct = direct_key_input_from_winit_code(
            KeyCode::Insert,
            ModifiersState::SHIFT,
            ModifierSideState::default(),
            false,
        )
        .expect("shift-insert maps to direct terminal input on Windows");

        assert_eq!(direct.input.key, TerminalKey::Insert);
        assert_eq!(
            direct.input.mods,
            KeyMods {
                shift: true,
                ..Default::default()
            }
        );
        assert_eq!(direct.suppress_egui_key, Some(egui::Key::Insert));
    }

    #[test]
    fn direct_input_synthesizes_left_command_when_modifier_state_is_stale() {
        let direct = direct_key_input_from_winit_code(
            KeyCode::KeyB,
            ModifiersState::empty(),
            ModifierSideState {
                left_command: true,
                ..Default::default()
            },
            false,
        )
        .expect("left super/command chord still maps to direct terminal input");

        assert_eq!(direct.input.key, TerminalKey::B);
        assert_eq!(
            direct.input.mods,
            KeyMods {
                command: true,
                ..Default::default()
            }
        );
        assert_eq!(direct.suppress_egui_key, Some(egui::Key::B));
    }

    #[test]
    fn direct_input_preserves_right_shift_tab_for_bootty_winit() {
        let mut side_state = ModifierSideState::default();
        side_state.update_key(KeyCode::ShiftRight, ElementState::Pressed);

        let direct = direct_key_input_from_winit_code(
            KeyCode::Tab,
            ModifiersState::SHIFT,
            side_state,
            false,
        )
        .expect("right shift tab uses direct terminal input");

        assert_eq!(direct.input.key, TerminalKey::Tab);
        assert_eq!(
            direct.input.mods,
            KeyMods {
                shift: true,
                right_shift: true,
                ..Default::default()
            }
        );
    }

    #[test]
    fn direct_input_applies_modifier_remaps_for_bootty_winit() {
        let mut remaps = ModifierRemapSet::default();
        remaps.parse("left_alt=right_ctrl").unwrap();
        remaps.finalize();

        let direct = direct_key_input_from_winit_code_with_remaps(
            KeyCode::Numpad1,
            ModifiersState::ALT,
            ModifierSideState::default(),
            false,
            &remaps,
        )
        .expect("numpad key maps to direct terminal input");

        assert_eq!(direct.input.key, TerminalKey::Numpad1);
        assert_eq!(
            direct.input.mods,
            KeyMods {
                ctrl: true,
                right_ctrl: true,
                ..Default::default()
            }
        );
    }

    #[test]
    fn direct_input_ignores_standalone_modifier_keys_for_bootty_winit() {
        assert!(
            direct_key_input_from_winit_code(
                KeyCode::ShiftLeft,
                ModifiersState::SHIFT,
                ModifierSideState::default(),
                false,
            )
            .is_none()
        );
        let mut side_state = ModifierSideState::default();
        side_state.update_key(KeyCode::AltRight, ElementState::Pressed);
        assert!(
            direct_key_input_from_winit_code(
                KeyCode::AltRight,
                ModifiersState::ALT,
                side_state,
                false
            )
            .is_none()
        );
    }

    #[test]
    fn direct_input_matches_physical_binding_trigger_for_bootty_winit() {
        let direct = direct_key_input_from_winit_code(
            KeyCode::Numpad1,
            ModifiersState::CONTROL,
            ModifierSideState::default(),
            false,
        )
        .expect("numpad key maps to direct terminal input");

        assert_eq!(
            BindingTrigger::from_key_input(direct.input),
            BindingTrigger {
                mods: BindingMods {
                    ctrl: true,
                    ..Default::default()
                },
                key: BindingKey::Physical(TerminalKey::Numpad1)
            }
        );
    }

    #[test]
    fn cmd_alt_letter_formats_as_trigger_for_keybind_recorder() {
        // egui collapses cmd+x into a Cut event with no key event, so the settings recorder builds
        // its trigger string from this direct input instead. cmd+alt+x must produce a trigger with
        // the alt preserved (the combo the recorder previously could not capture). The serializer
        // emits the canonical uppercase letter; the app lowercases it to match the default keybinds.
        let direct = direct_key_input_from_winit_code(
            KeyCode::KeyX,
            ModifiersState::SUPER | ModifiersState::ALT,
            ModifierSideState::default(),
            false,
        )
        .expect("cmd+alt+x maps to direct terminal input");

        assert_eq!(
            BindingTrigger::from_key_input(direct.input).format_entry(),
            "cmd+alt+KeyX"
        );
    }
}
