use eframe::egui::{self, Pos2};
use winit::{
    event::ElementState,
    keyboard::{KeyCode, ModifiersState},
};

use crate::{
    geometry::TerminalSurface,
    modifier_remap::ModifierRemapSet,
    terminal::{KeyInput, KeyMods, MouseAction, MouseButton, MouseInput, TerminalKey},
};

pub fn key_mods_from_egui_modifiers(modifiers: egui::Modifiers) -> KeyMods {
    KeyMods {
        shift: modifiers.shift,
        alt: modifiers.alt,
        ctrl: modifiers.ctrl,
        command: modifiers.command,
        ..Default::default()
    }
}

pub fn mouse_mods_from_egui_modifiers(modifiers: egui::Modifiers) -> KeyMods {
    KeyMods {
        shift: modifiers.shift,
        alt: modifiers.alt,
        ctrl: modifiers.ctrl,
        command: false,
        ..Default::default()
    }
}

pub fn key_mods_from_winit_modifiers(modifiers: ModifiersState) -> KeyMods {
    KeyMods {
        shift: modifiers.shift_key(),
        alt: modifiers.alt_key(),
        ctrl: modifiers.control_key(),
        command: modifiers.super_key(),
        ..Default::default()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModifierSideState {
    pub left_shift: bool,
    pub right_shift: bool,
    pub left_alt: bool,
    pub right_alt: bool,
    pub left_ctrl: bool,
    pub right_ctrl: bool,
    pub left_command: bool,
    pub right_command: bool,
}

impl ModifierSideState {
    pub fn update_key(&mut self, code: KeyCode, state: ElementState) {
        let pressed = state == ElementState::Pressed;
        match code {
            KeyCode::ShiftLeft => self.left_shift = pressed,
            KeyCode::ShiftRight => self.right_shift = pressed,
            KeyCode::AltLeft => self.left_alt = pressed,
            KeyCode::AltRight => self.right_alt = pressed,
            KeyCode::ControlLeft => self.left_ctrl = pressed,
            KeyCode::ControlRight => self.right_ctrl = pressed,
            KeyCode::SuperLeft => self.left_command = pressed,
            KeyCode::SuperRight => self.right_command = pressed,
            _ => {}
        }
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn retain_active_modifiers(&mut self, modifiers: ModifiersState) {
        if !modifiers.shift_key() {
            self.left_shift = false;
            self.right_shift = false;
        }
        if !modifiers.alt_key() {
            self.left_alt = false;
            self.right_alt = false;
        }
        if !modifiers.control_key() {
            self.left_ctrl = false;
            self.right_ctrl = false;
        }
        if !modifiers.super_key() {
            self.left_command = false;
            self.right_command = false;
        }
    }

    pub fn apply_to_key_input(self, input: &mut KeyInput) {
        input.mods.shift = input.mods.shift || self.left_shift || self.right_shift;
        input.mods.alt = input.mods.alt || self.left_alt || self.right_alt;
        input.mods.ctrl = input.mods.ctrl || self.left_ctrl || self.right_ctrl;
        input.mods.command = input.mods.command || self.left_command || self.right_command;
        input.mods.right_shift = input.mods.shift && self.right_shift;
        input.mods.right_alt = input.mods.alt && self.right_alt;
        input.mods.right_ctrl = input.mods.ctrl && self.right_ctrl;
        input.mods.right_command = input.mods.command && self.right_command;
    }

    pub(crate) fn has_alt(self) -> bool {
        self.left_alt || self.right_alt
    }

    pub(crate) fn has_right_shift(self) -> bool {
        self.right_shift
    }

    pub(crate) fn has_command(self) -> bool {
        self.left_command || self.right_command
    }
}

pub fn bare_terminal_key_input(
    code: KeyCode,
    modifiers: ModifiersState,
    repeat: bool,
) -> Option<KeyInput> {
    bare_terminal_key_input_with_remaps(code, modifiers, repeat, &ModifierRemapSet::default())
}

pub fn bare_terminal_key_input_with_remaps(
    code: KeyCode,
    modifiers: ModifiersState,
    repeat: bool,
    modifier_remaps: &ModifierRemapSet,
) -> Option<KeyInput> {
    bare_terminal_key_input_with_sides_and_remaps(
        code,
        modifiers,
        ModifierSideState::default(),
        repeat,
        modifier_remaps,
    )
}

pub fn bare_terminal_key_input_with_sides_and_remaps(
    code: KeyCode,
    modifiers: ModifiersState,
    side_state: ModifierSideState,
    repeat: bool,
    modifier_remaps: &ModifierRemapSet,
) -> Option<KeyInput> {
    let mut input = bare_terminal_key_input_with_sides(code, modifiers, side_state, repeat)?;
    input.mods = modifier_remaps.apply(input.mods);
    Some(input)
}

#[cfg(any(feature = "bare-host", test))]
pub fn bare_terminal_paste_shortcut(code: KeyCode, modifiers: ModifiersState) -> bool {
    if code != KeyCode::KeyV {
        return false;
    }
    let platform_paste = if cfg!(target_os = "macos") {
        modifiers.super_key()
    } else {
        modifiers.control_key()
    };
    platform_paste && !modifiers.alt_key()
}

pub fn bare_terminal_key_input_with_sides(
    code: KeyCode,
    modifiers: ModifiersState,
    side_state: ModifierSideState,
    repeat: bool,
) -> Option<KeyInput> {
    let key = terminal_key_from_winit_code(code)?;
    let mut input = KeyInput {
        key,
        mods: key_mods_from_winit_modifiers(modifiers),
        repeat,
        utf8: physical_key_utf8(key, modifiers.shift_key()),
        unshifted: key_unshifted(key),
    };
    side_state.apply_to_key_input(&mut input);
    Some(input)
}

fn terminal_key_from_winit_code(code: KeyCode) -> Option<TerminalKey> {
    Some(match code {
        KeyCode::Backquote => TerminalKey::Backquote,
        KeyCode::Backslash => TerminalKey::Backslash,
        KeyCode::BracketLeft => TerminalKey::BracketLeft,
        KeyCode::BracketRight => TerminalKey::BracketRight,
        KeyCode::Comma => TerminalKey::Comma,
        KeyCode::Digit0 => TerminalKey::Digit0,
        KeyCode::Digit1 => TerminalKey::Digit1,
        KeyCode::Digit2 => TerminalKey::Digit2,
        KeyCode::Digit3 => TerminalKey::Digit3,
        KeyCode::Digit4 => TerminalKey::Digit4,
        KeyCode::Digit5 => TerminalKey::Digit5,
        KeyCode::Digit6 => TerminalKey::Digit6,
        KeyCode::Digit7 => TerminalKey::Digit7,
        KeyCode::Digit8 => TerminalKey::Digit8,
        KeyCode::Digit9 => TerminalKey::Digit9,
        KeyCode::Equal => TerminalKey::Equal,
        KeyCode::KeyA => TerminalKey::A,
        KeyCode::KeyB => TerminalKey::B,
        KeyCode::KeyC => TerminalKey::C,
        KeyCode::KeyD => TerminalKey::D,
        KeyCode::KeyE => TerminalKey::E,
        KeyCode::KeyF => TerminalKey::F,
        KeyCode::KeyG => TerminalKey::G,
        KeyCode::KeyH => TerminalKey::H,
        KeyCode::KeyI => TerminalKey::I,
        KeyCode::KeyJ => TerminalKey::J,
        KeyCode::KeyK => TerminalKey::K,
        KeyCode::KeyL => TerminalKey::L,
        KeyCode::KeyM => TerminalKey::M,
        KeyCode::KeyN => TerminalKey::N,
        KeyCode::KeyO => TerminalKey::O,
        KeyCode::KeyP => TerminalKey::P,
        KeyCode::KeyQ => TerminalKey::Q,
        KeyCode::KeyR => TerminalKey::R,
        KeyCode::KeyS => TerminalKey::S,
        KeyCode::KeyT => TerminalKey::T,
        KeyCode::KeyU => TerminalKey::U,
        KeyCode::KeyV => TerminalKey::V,
        KeyCode::KeyW => TerminalKey::W,
        KeyCode::KeyX => TerminalKey::X,
        KeyCode::KeyY => TerminalKey::Y,
        KeyCode::KeyZ => TerminalKey::Z,
        KeyCode::Minus => TerminalKey::Minus,
        KeyCode::Period => TerminalKey::Period,
        KeyCode::Quote => TerminalKey::Quote,
        KeyCode::Semicolon => TerminalKey::Semicolon,
        KeyCode::Slash => TerminalKey::Slash,
        KeyCode::Enter => TerminalKey::Enter,
        KeyCode::NumpadEnter => TerminalKey::NumpadEnter,
        KeyCode::Tab => TerminalKey::Tab,
        KeyCode::Backspace => TerminalKey::Backspace,
        KeyCode::Escape => TerminalKey::Escape,
        KeyCode::ArrowUp => TerminalKey::ArrowUp,
        KeyCode::ArrowDown => TerminalKey::ArrowDown,
        KeyCode::ArrowRight => TerminalKey::ArrowRight,
        KeyCode::ArrowLeft => TerminalKey::ArrowLeft,
        KeyCode::Delete => TerminalKey::Delete,
        KeyCode::Home => TerminalKey::Home,
        KeyCode::End => TerminalKey::End,
        KeyCode::PageUp => TerminalKey::PageUp,
        KeyCode::PageDown => TerminalKey::PageDown,
        KeyCode::Space => TerminalKey::Space,
        KeyCode::Insert => TerminalKey::Insert,
        KeyCode::F1 => TerminalKey::F1,
        KeyCode::F2 => TerminalKey::F2,
        KeyCode::F3 => TerminalKey::F3,
        KeyCode::F4 => TerminalKey::F4,
        KeyCode::F5 => TerminalKey::F5,
        KeyCode::F6 => TerminalKey::F6,
        KeyCode::F7 => TerminalKey::F7,
        KeyCode::F8 => TerminalKey::F8,
        KeyCode::F9 => TerminalKey::F9,
        KeyCode::F10 => TerminalKey::F10,
        KeyCode::F11 => TerminalKey::F11,
        KeyCode::F12 => TerminalKey::F12,
        KeyCode::Numpad0 => TerminalKey::Numpad0,
        KeyCode::Numpad1 => TerminalKey::Numpad1,
        KeyCode::Numpad2 => TerminalKey::Numpad2,
        KeyCode::Numpad3 => TerminalKey::Numpad3,
        KeyCode::Numpad4 => TerminalKey::Numpad4,
        KeyCode::Numpad5 => TerminalKey::Numpad5,
        KeyCode::Numpad6 => TerminalKey::Numpad6,
        KeyCode::Numpad7 => TerminalKey::Numpad7,
        KeyCode::Numpad8 => TerminalKey::Numpad8,
        KeyCode::Numpad9 => TerminalKey::Numpad9,
        KeyCode::NumpadAdd => TerminalKey::NumpadAdd,
        KeyCode::NumpadDecimal => TerminalKey::NumpadDecimal,
        KeyCode::NumpadDivide => TerminalKey::NumpadDivide,
        KeyCode::NumpadEqual => TerminalKey::NumpadEqual,
        KeyCode::NumpadMultiply => TerminalKey::NumpadMultiply,
        KeyCode::NumpadSubtract => TerminalKey::NumpadSubtract,
        _ => return None,
    })
}

pub fn mouse_input_from_surface(
    pos: Pos2,
    action: MouseAction,
    button: Option<MouseButton>,
    mods: KeyMods,
    surface: TerminalSurface,
) -> Option<MouseInput> {
    let position = surface.relative_position(pos)?;

    Some(MouseInput {
        action,
        button,
        mods,
        x: position.x,
        y: position.y,
        size: surface.mouse_metrics().into(),
    })
}

pub fn is_control_key(key: TerminalKey) -> bool {
    matches!(
        key,
        TerminalKey::Enter
            | TerminalKey::Tab
            | TerminalKey::Backspace
            | TerminalKey::Escape
            | TerminalKey::Insert
            | TerminalKey::Delete
            | TerminalKey::Home
            | TerminalKey::End
            | TerminalKey::PageUp
            | TerminalKey::PageDown
            | TerminalKey::ArrowUp
            | TerminalKey::ArrowDown
            | TerminalKey::ArrowRight
            | TerminalKey::ArrowLeft
            | TerminalKey::F1
            | TerminalKey::F2
            | TerminalKey::F3
            | TerminalKey::F4
            | TerminalKey::F5
            | TerminalKey::F6
            | TerminalKey::F7
            | TerminalKey::F8
            | TerminalKey::F9
            | TerminalKey::F10
            | TerminalKey::F11
            | TerminalKey::F12
    )
}

pub fn egui_key_utf8(key: TerminalKey, shifted: bool) -> Option<&'static str> {
    key_text(key).map(|text| {
        if shifted {
            text.shifted_letter_utf8.unwrap_or(text.unshifted_utf8)
        } else {
            text.unshifted_utf8
        }
    })
}

pub fn physical_key_utf8(key: TerminalKey, shifted: bool) -> Option<&'static str> {
    key_text(key).map(|text| {
        if shifted {
            text.shifted_utf8.unwrap_or(text.unshifted_utf8)
        } else {
            text.unshifted_utf8
        }
    })
}

pub fn key_unshifted(key: TerminalKey) -> Option<char> {
    key_text(key).map(|text| text.unshifted)
}

pub fn mouse_wheel_button_from_delta_y(delta_y: f32) -> Option<MouseButton> {
    if delta_y > 0.0 {
        Some(MouseButton::Four)
    } else if delta_y < 0.0 {
        Some(MouseButton::Five)
    } else {
        None
    }
}

struct KeyText {
    unshifted: char,
    unshifted_utf8: &'static str,
    shifted_utf8: Option<&'static str>,
    shifted_letter_utf8: Option<&'static str>,
}

fn key_text(key: TerminalKey) -> Option<KeyText> {
    let (unshifted, unshifted_utf8, shifted_utf8) = match key {
        TerminalKey::Space => (' ', " ", None),
        TerminalKey::Backquote => ('`', "`", Some("~")),
        TerminalKey::Backslash => ('\\', "\\", Some("|")),
        TerminalKey::BracketLeft => ('[', "[", Some("{")),
        TerminalKey::BracketRight => (']', "]", Some("}")),
        TerminalKey::Comma => (',', ",", Some("<")),
        TerminalKey::Digit0 => ('0', "0", Some(")")),
        TerminalKey::Digit1 => ('1', "1", Some("!")),
        TerminalKey::Digit2 => ('2', "2", Some("@")),
        TerminalKey::Digit3 => ('3', "3", Some("#")),
        TerminalKey::Digit4 => ('4', "4", Some("$")),
        TerminalKey::Digit5 => ('5', "5", Some("%")),
        TerminalKey::Digit6 => ('6', "6", Some("^")),
        TerminalKey::Digit7 => ('7', "7", Some("&")),
        TerminalKey::Digit8 => ('8', "8", Some("*")),
        TerminalKey::Digit9 => ('9', "9", Some("(")),
        TerminalKey::Equal => ('=', "=", Some("+")),
        TerminalKey::Minus => ('-', "-", Some("_")),
        TerminalKey::Numpad0 => ('0', "0", None),
        TerminalKey::Numpad1 => ('1', "1", None),
        TerminalKey::Numpad2 => ('2', "2", None),
        TerminalKey::Numpad3 => ('3', "3", None),
        TerminalKey::Numpad4 => ('4', "4", None),
        TerminalKey::Numpad5 => ('5', "5", None),
        TerminalKey::Numpad6 => ('6', "6", None),
        TerminalKey::Numpad7 => ('7', "7", None),
        TerminalKey::Numpad8 => ('8', "8", None),
        TerminalKey::Numpad9 => ('9', "9", None),
        TerminalKey::NumpadAdd => ('+', "+", None),
        TerminalKey::NumpadDecimal => ('.', ".", None),
        TerminalKey::NumpadDivide => ('/', "/", None),
        TerminalKey::NumpadEqual => ('=', "=", None),
        TerminalKey::NumpadMultiply => ('*', "*", None),
        TerminalKey::NumpadSubtract => ('-', "-", None),
        TerminalKey::Period => ('.', ".", Some(">")),
        TerminalKey::Quote => ('\'', "'", Some("\"")),
        TerminalKey::Semicolon => (';', ";", Some(":")),
        TerminalKey::Slash => ('/', "/", Some("?")),
        TerminalKey::A => return Some(letter_text('a', "a", "A")),
        TerminalKey::B => return Some(letter_text('b', "b", "B")),
        TerminalKey::C => return Some(letter_text('c', "c", "C")),
        TerminalKey::D => return Some(letter_text('d', "d", "D")),
        TerminalKey::E => return Some(letter_text('e', "e", "E")),
        TerminalKey::F => return Some(letter_text('f', "f", "F")),
        TerminalKey::G => return Some(letter_text('g', "g", "G")),
        TerminalKey::H => return Some(letter_text('h', "h", "H")),
        TerminalKey::I => return Some(letter_text('i', "i", "I")),
        TerminalKey::J => return Some(letter_text('j', "j", "J")),
        TerminalKey::K => return Some(letter_text('k', "k", "K")),
        TerminalKey::L => return Some(letter_text('l', "l", "L")),
        TerminalKey::M => return Some(letter_text('m', "m", "M")),
        TerminalKey::N => return Some(letter_text('n', "n", "N")),
        TerminalKey::O => return Some(letter_text('o', "o", "O")),
        TerminalKey::P => return Some(letter_text('p', "p", "P")),
        TerminalKey::Q => return Some(letter_text('q', "q", "Q")),
        TerminalKey::R => return Some(letter_text('r', "r", "R")),
        TerminalKey::S => return Some(letter_text('s', "s", "S")),
        TerminalKey::T => return Some(letter_text('t', "t", "T")),
        TerminalKey::U => return Some(letter_text('u', "u", "U")),
        TerminalKey::V => return Some(letter_text('v', "v", "V")),
        TerminalKey::W => return Some(letter_text('w', "w", "W")),
        TerminalKey::X => return Some(letter_text('x', "x", "X")),
        TerminalKey::Y => return Some(letter_text('y', "y", "Y")),
        TerminalKey::Z => return Some(letter_text('z', "z", "Z")),
        _ => return None,
    };
    Some(KeyText {
        unshifted,
        unshifted_utf8,
        shifted_utf8,
        shifted_letter_utf8: None,
    })
}

fn letter_text(
    unshifted: char,
    unshifted_utf8: &'static str,
    shifted_utf8: &'static str,
) -> KeyText {
    KeyText {
        unshifted,
        unshifted_utf8,
        shifted_utf8: Some(shifted_utf8),
        shifted_letter_utf8: Some(shifted_utf8),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_side_state_drops_sides_when_aggregate_modifier_is_released() {
        let mut state = ModifierSideState {
            left_shift: true,
            right_alt: true,
            left_ctrl: true,
            left_command: true,
            right_command: true,
            ..Default::default()
        };

        state.retain_active_modifiers(ModifiersState::ALT);

        assert_eq!(
            state,
            ModifierSideState {
                right_alt: true,
                ..Default::default()
            }
        );
    }
}
