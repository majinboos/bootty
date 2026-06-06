use eframe::egui::{self, Pos2};
use winit::keyboard::ModifiersState;

use crate::{
    geometry::TerminalSurface,
    terminal::{KeyMods, MouseAction, MouseButton, MouseEncoderSize, MouseInput, TerminalKey},
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

pub fn mouse_input_from_surface(
    pos: Pos2,
    action: MouseAction,
    button: Option<MouseButton>,
    mods: KeyMods,
    surface: TerminalSurface,
) -> Option<MouseInput> {
    let position = surface.relative_position(pos)?;
    let metrics = surface.mouse_metrics();

    Some(MouseInput {
        action,
        button,
        mods,
        x: position.x,
        y: position.y,
        size: MouseEncoderSize {
            screen_width: metrics.screen_width,
            screen_height: metrics.screen_height,
            cell_width: metrics.cell_width.max(1),
            cell_height: metrics.cell_height.max(1),
            padding_top: metrics.padding.top,
            padding_bottom: metrics.padding.bottom,
            padding_right: metrics.padding.right,
            padding_left: metrics.padding.left,
        },
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
