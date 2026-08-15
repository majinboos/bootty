use eframe::egui;

use bootty_terminal::{
    terminal_engine::{TerminalCopyModeAction, TerminalCopyModeMotion, TerminalSearchDirection},
    terminal_input_model::{KeyInput, TerminalKey},
};

pub(super) fn copy_shortcut_pressed(event: &egui::Event) -> bool {
    matches!(
        event,
        egui::Event::Key {
            key: egui::Key::C,
            pressed: true,
            repeat: false,
            modifiers,
            ..
        } if modifiers.command && !modifiers.ctrl && !modifiers.alt
    )
}

pub(super) fn direct_copy_shortcut_pressed(input: KeyInput) -> bool {
    input.key == TerminalKey::C
        && input.mods.command
        && !input.mods.ctrl
        && !input.mods.alt
        && !input.repeat
}

pub(super) fn copy_mode_key_input_present(events: &[egui::Event]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, egui::Event::Key { .. } | egui::Event::Text(_)))
}

pub(super) fn copy_mode_egui_key_should_pass_to_app(
    key: egui::Key,
    modifiers: egui::Modifiers,
) -> bool {
    modifiers.alt || (modifiers.command && !copy_mode_egui_key_is_copy_shortcut(key, modifiers))
}

fn copy_mode_egui_key_is_copy_shortcut(key: egui::Key, modifiers: egui::Modifiers) -> bool {
    key == egui::Key::C && modifiers.command && !modifiers.ctrl && !modifiers.alt
}

pub(super) fn copy_mode_input_should_pass_to_app(input: KeyInput) -> bool {
    input.mods.alt || (input.mods.command && !direct_copy_shortcut_pressed(input))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CopyModeKeyAction {
    Terminal(TerminalCopyModeAction),
    SearchPrompt(TerminalSearchDirection),
    SearchWord(TerminalSearchDirection),
    SearchRepeat(CopyModeSearchRepeat),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CopyModeSearchRepeat {
    SameDirection,
    OppositeDirection,
}

impl CopyModeSearchRepeat {
    pub(super) fn direction(
        self,
        started_direction: TerminalSearchDirection,
    ) -> TerminalSearchDirection {
        match self {
            Self::SameDirection => started_direction,
            Self::OppositeDirection => opposite_terminal_search_direction(started_direction),
        }
    }
}

fn opposite_terminal_search_direction(
    direction: TerminalSearchDirection,
) -> TerminalSearchDirection {
    match direction {
        TerminalSearchDirection::Previous => TerminalSearchDirection::Next,
        TerminalSearchDirection::Current | TerminalSearchDirection::Next => {
            TerminalSearchDirection::Previous
        }
    }
}

fn copy_mode_terminal_action(action: TerminalCopyModeAction) -> Option<CopyModeKeyAction> {
    Some(CopyModeKeyAction::Terminal(action))
}

pub(super) fn copy_mode_action_for_egui_event(
    event: &egui::Event,
    suppress_next_text: &mut bool,
) -> Option<CopyModeKeyAction> {
    match event {
        egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => {
            let action = copy_mode_action_for_egui_key(*key, *modifiers);
            *suppress_next_text = action.is_some() && copy_mode_egui_key_may_emit_text(*key);
            action
        }
        egui::Event::Text(text) => {
            if std::mem::take(suppress_next_text) {
                None
            } else {
                text.chars().find_map(copy_mode_action_for_char)
            }
        }
        _ => None,
    }
}

pub(super) fn copy_mode_egui_key_may_emit_text(key: egui::Key) -> bool {
    matches!(
        key,
        egui::Key::Questionmark
            | egui::Key::Slash
            | egui::Key::A
            | egui::Key::B
            | egui::Key::C
            | egui::Key::D
            | egui::Key::E
            | egui::Key::F
            | egui::Key::G
            | egui::Key::H
            | egui::Key::I
            | egui::Key::J
            | egui::Key::K
            | egui::Key::L
            | egui::Key::M
            | egui::Key::N
            | egui::Key::O
            | egui::Key::P
            | egui::Key::Q
            | egui::Key::R
            | egui::Key::S
            | egui::Key::T
            | egui::Key::U
            | egui::Key::V
            | egui::Key::W
            | egui::Key::X
            | egui::Key::Y
            | egui::Key::Z
            | egui::Key::Num0
            | egui::Key::Num1
            | egui::Key::Num2
            | egui::Key::Num3
            | egui::Key::Num4
            | egui::Key::Num5
            | egui::Key::Num6
            | egui::Key::Num7
            | egui::Key::Num8
            | egui::Key::Num9
            | egui::Key::Space
    )
}

pub(super) fn copy_mode_action_for_egui_key(
    key: egui::Key,
    modifiers: egui::Modifiers,
) -> Option<CopyModeKeyAction> {
    if key == egui::Key::C && modifiers.command && !modifiers.ctrl && !modifiers.alt {
        return copy_mode_terminal_action(TerminalCopyModeAction::CopySelectionAndCancel);
    }
    if modifiers.command || modifiers.alt {
        return None;
    }
    if modifiers.ctrl {
        return copy_mode_ctrl_key_action(key);
    }
    match key {
        egui::Key::Questionmark => {
            return Some(CopyModeKeyAction::SearchPrompt(
                TerminalSearchDirection::Previous,
            ));
        }
        egui::Key::Slash if modifiers.shift => {
            return Some(CopyModeKeyAction::SearchPrompt(
                TerminalSearchDirection::Previous,
            ));
        }
        egui::Key::Slash => {
            return Some(CopyModeKeyAction::SearchPrompt(
                TerminalSearchDirection::Next,
            ));
        }
        egui::Key::Num3 if modifiers.shift => {
            return Some(CopyModeKeyAction::SearchWord(
                TerminalSearchDirection::Previous,
            ));
        }
        egui::Key::Num8 if modifiers.shift => {
            return Some(CopyModeKeyAction::SearchWord(TerminalSearchDirection::Next));
        }
        _ => {}
    }
    if modifiers.shift {
        return match key {
            egui::Key::G => copy_mode_motion(TerminalCopyModeMotion::HistoryBottom),
            egui::Key::H => copy_mode_motion(TerminalCopyModeMotion::TopLine),
            egui::Key::L => copy_mode_motion(TerminalCopyModeMotion::BottomLine),
            egui::Key::M => copy_mode_motion(TerminalCopyModeMotion::MiddleLine),
            egui::Key::V => copy_mode_terminal_action(TerminalCopyModeAction::SelectLine),
            egui::Key::Num4 => copy_mode_motion(TerminalCopyModeMotion::EndOfLine),
            egui::Key::Num6 => copy_mode_motion(TerminalCopyModeMotion::BackToIndentation),
            _ => None,
        };
    }
    match key {
        egui::Key::Escape => {
            copy_mode_terminal_action(TerminalCopyModeAction::CancelOrClearSelection)
        }
        egui::Key::Enter => {
            copy_mode_terminal_action(TerminalCopyModeAction::CopySelectionAndCancel)
        }
        egui::Key::Space => copy_mode_terminal_action(TerminalCopyModeAction::BeginSelection),
        egui::Key::ArrowLeft => copy_mode_motion(TerminalCopyModeMotion::Left),
        egui::Key::ArrowRight => copy_mode_motion(TerminalCopyModeMotion::Right),
        egui::Key::ArrowUp => copy_mode_motion(TerminalCopyModeMotion::Up),
        egui::Key::ArrowDown => copy_mode_motion(TerminalCopyModeMotion::Down),
        egui::Key::PageUp => copy_mode_motion(TerminalCopyModeMotion::PageUp),
        egui::Key::PageDown => copy_mode_motion(TerminalCopyModeMotion::PageDown),
        egui::Key::Home => copy_mode_motion(TerminalCopyModeMotion::StartOfLine),
        egui::Key::End => copy_mode_motion(TerminalCopyModeMotion::EndOfLine),
        egui::Key::H => copy_mode_motion(TerminalCopyModeMotion::Left),
        egui::Key::J => copy_mode_motion(TerminalCopyModeMotion::Down),
        egui::Key::K => copy_mode_motion(TerminalCopyModeMotion::Up),
        egui::Key::L => copy_mode_motion(TerminalCopyModeMotion::Right),
        egui::Key::N => Some(CopyModeKeyAction::SearchRepeat(
            CopyModeSearchRepeat::SameDirection,
        )),
        egui::Key::G => copy_mode_motion(TerminalCopyModeMotion::HistoryTop),
        egui::Key::W => copy_mode_motion(TerminalCopyModeMotion::NextWord),
        egui::Key::B => copy_mode_motion(TerminalCopyModeMotion::PreviousWord),
        egui::Key::E => copy_mode_motion(TerminalCopyModeMotion::NextWordEnd),
        egui::Key::V => copy_mode_terminal_action(TerminalCopyModeAction::ToggleSelection),
        egui::Key::O => copy_mode_terminal_action(TerminalCopyModeAction::ToggleSelectionEnd),
        egui::Key::Y => copy_mode_terminal_action(TerminalCopyModeAction::CopySelectionAndCancel),
        egui::Key::Q => copy_mode_terminal_action(TerminalCopyModeAction::Cancel),
        egui::Key::Num0 => copy_mode_motion(TerminalCopyModeMotion::StartOfLine),
        _ => None,
    }
}

pub(super) fn copy_mode_action_for_input(input: KeyInput) -> Option<CopyModeKeyAction> {
    if direct_copy_shortcut_pressed(input) {
        return copy_mode_terminal_action(TerminalCopyModeAction::CopySelectionAndCancel);
    }
    if input.mods.command || input.mods.alt {
        return None;
    }
    if input.mods.ctrl {
        return copy_mode_ctrl_terminal_key_action(input.key);
    }
    if input.mods.shift {
        return copy_mode_shift_terminal_key_action(input.key).or_else(|| {
            input
                .utf8
                .and_then(single_char)
                .and_then(copy_mode_action_for_char)
        });
    }
    copy_mode_terminal_key_action(input.key).or_else(|| {
        input
            .utf8
            .and_then(single_char)
            .and_then(copy_mode_action_for_char)
    })
}

fn single_char(value: &str) -> Option<char> {
    let mut chars = value.chars();
    let ch = chars.next()?;
    chars.next().is_none().then_some(ch)
}

fn copy_mode_ctrl_key_action(key: egui::Key) -> Option<CopyModeKeyAction> {
    match key {
        egui::Key::B => copy_mode_motion(TerminalCopyModeMotion::PageUp),
        egui::Key::C | egui::Key::G => copy_mode_terminal_action(TerminalCopyModeAction::Cancel),
        egui::Key::D => copy_mode_motion(TerminalCopyModeMotion::HalfPageDown),
        egui::Key::E => copy_mode_motion(TerminalCopyModeMotion::ScrollDown),
        egui::Key::F => copy_mode_motion(TerminalCopyModeMotion::PageDown),
        egui::Key::J => copy_mode_terminal_action(TerminalCopyModeAction::CopySelectionAndCancel),
        egui::Key::N => copy_mode_motion(TerminalCopyModeMotion::Down),
        egui::Key::P => copy_mode_motion(TerminalCopyModeMotion::Up),
        egui::Key::U => copy_mode_motion(TerminalCopyModeMotion::HalfPageUp),
        egui::Key::V => copy_mode_terminal_action(TerminalCopyModeAction::ToggleRectangle),
        egui::Key::Y => copy_mode_motion(TerminalCopyModeMotion::ScrollUp),
        _ => None,
    }
}

fn copy_mode_ctrl_terminal_key_action(key: TerminalKey) -> Option<CopyModeKeyAction> {
    match key {
        TerminalKey::B => copy_mode_motion(TerminalCopyModeMotion::PageUp),
        TerminalKey::C | TerminalKey::G => {
            copy_mode_terminal_action(TerminalCopyModeAction::Cancel)
        }
        TerminalKey::D => copy_mode_motion(TerminalCopyModeMotion::HalfPageDown),
        TerminalKey::E => copy_mode_motion(TerminalCopyModeMotion::ScrollDown),
        TerminalKey::F => copy_mode_motion(TerminalCopyModeMotion::PageDown),
        TerminalKey::J | TerminalKey::Enter | TerminalKey::NumpadEnter => {
            copy_mode_terminal_action(TerminalCopyModeAction::CopySelectionAndCancel)
        }
        TerminalKey::N => copy_mode_motion(TerminalCopyModeMotion::Down),
        TerminalKey::P => copy_mode_motion(TerminalCopyModeMotion::Up),
        TerminalKey::U => copy_mode_motion(TerminalCopyModeMotion::HalfPageUp),
        TerminalKey::V => copy_mode_terminal_action(TerminalCopyModeAction::ToggleRectangle),
        TerminalKey::Y => copy_mode_motion(TerminalCopyModeMotion::ScrollUp),
        _ => None,
    }
}

fn copy_mode_shift_terminal_key_action(key: TerminalKey) -> Option<CopyModeKeyAction> {
    match key {
        TerminalKey::G => copy_mode_motion(TerminalCopyModeMotion::HistoryBottom),
        TerminalKey::H => copy_mode_motion(TerminalCopyModeMotion::TopLine),
        TerminalKey::L => copy_mode_motion(TerminalCopyModeMotion::BottomLine),
        TerminalKey::M => copy_mode_motion(TerminalCopyModeMotion::MiddleLine),
        TerminalKey::V => copy_mode_terminal_action(TerminalCopyModeAction::SelectLine),
        TerminalKey::Digit4 => copy_mode_motion(TerminalCopyModeMotion::EndOfLine),
        TerminalKey::Digit6 => copy_mode_motion(TerminalCopyModeMotion::BackToIndentation),
        _ => None,
    }
}

fn copy_mode_terminal_key_action(key: TerminalKey) -> Option<CopyModeKeyAction> {
    match key {
        TerminalKey::Escape => {
            copy_mode_terminal_action(TerminalCopyModeAction::CancelOrClearSelection)
        }
        TerminalKey::Enter | TerminalKey::NumpadEnter => {
            copy_mode_terminal_action(TerminalCopyModeAction::CopySelectionAndCancel)
        }
        TerminalKey::Space => copy_mode_terminal_action(TerminalCopyModeAction::BeginSelection),
        TerminalKey::ArrowLeft => copy_mode_motion(TerminalCopyModeMotion::Left),
        TerminalKey::ArrowRight => copy_mode_motion(TerminalCopyModeMotion::Right),
        TerminalKey::ArrowUp => copy_mode_motion(TerminalCopyModeMotion::Up),
        TerminalKey::ArrowDown => copy_mode_motion(TerminalCopyModeMotion::Down),
        TerminalKey::PageUp => copy_mode_motion(TerminalCopyModeMotion::PageUp),
        TerminalKey::PageDown => copy_mode_motion(TerminalCopyModeMotion::PageDown),
        TerminalKey::Home => copy_mode_motion(TerminalCopyModeMotion::StartOfLine),
        TerminalKey::End => copy_mode_motion(TerminalCopyModeMotion::EndOfLine),
        TerminalKey::H => copy_mode_motion(TerminalCopyModeMotion::Left),
        TerminalKey::J => copy_mode_motion(TerminalCopyModeMotion::Down),
        TerminalKey::K => copy_mode_motion(TerminalCopyModeMotion::Up),
        TerminalKey::N => Some(CopyModeKeyAction::SearchRepeat(
            CopyModeSearchRepeat::SameDirection,
        )),
        TerminalKey::L => copy_mode_motion(TerminalCopyModeMotion::Right),
        TerminalKey::G => copy_mode_motion(TerminalCopyModeMotion::HistoryTop),
        TerminalKey::W => copy_mode_motion(TerminalCopyModeMotion::NextWord),
        TerminalKey::B => copy_mode_motion(TerminalCopyModeMotion::PreviousWord),
        TerminalKey::E => copy_mode_motion(TerminalCopyModeMotion::NextWordEnd),
        TerminalKey::V => copy_mode_terminal_action(TerminalCopyModeAction::ToggleSelection),
        TerminalKey::O => copy_mode_terminal_action(TerminalCopyModeAction::ToggleSelectionEnd),
        TerminalKey::Y => copy_mode_terminal_action(TerminalCopyModeAction::CopySelectionAndCancel),
        TerminalKey::Q => copy_mode_terminal_action(TerminalCopyModeAction::Cancel),
        TerminalKey::Digit0 | TerminalKey::Numpad0 => {
            copy_mode_motion(TerminalCopyModeMotion::StartOfLine)
        }
        _ => None,
    }
}

pub(super) fn copy_mode_action_for_char(ch: char) -> Option<CopyModeKeyAction> {
    match ch {
        '/' => Some(CopyModeKeyAction::SearchPrompt(
            TerminalSearchDirection::Next,
        )),
        '?' => Some(CopyModeKeyAction::SearchPrompt(
            TerminalSearchDirection::Previous,
        )),
        '*' => Some(CopyModeKeyAction::SearchWord(TerminalSearchDirection::Next)),
        '#' => Some(CopyModeKeyAction::SearchWord(
            TerminalSearchDirection::Previous,
        )),
        '$' => copy_mode_motion(TerminalCopyModeMotion::EndOfLine),
        '^' => copy_mode_motion(TerminalCopyModeMotion::BackToIndentation),
        '0' => copy_mode_motion(TerminalCopyModeMotion::StartOfLine),
        'b' => copy_mode_motion(TerminalCopyModeMotion::PreviousWord),
        'e' => copy_mode_motion(TerminalCopyModeMotion::NextWordEnd),
        'g' => copy_mode_motion(TerminalCopyModeMotion::HistoryTop),
        'G' => copy_mode_motion(TerminalCopyModeMotion::HistoryBottom),
        'h' => copy_mode_motion(TerminalCopyModeMotion::Left),
        'H' => copy_mode_motion(TerminalCopyModeMotion::TopLine),
        'j' => copy_mode_motion(TerminalCopyModeMotion::Down),
        'k' => copy_mode_motion(TerminalCopyModeMotion::Up),
        'l' => copy_mode_motion(TerminalCopyModeMotion::Right),
        'L' => copy_mode_motion(TerminalCopyModeMotion::BottomLine),
        'M' => copy_mode_motion(TerminalCopyModeMotion::MiddleLine),
        'n' => Some(CopyModeKeyAction::SearchRepeat(
            CopyModeSearchRepeat::SameDirection,
        )),
        'N' => Some(CopyModeKeyAction::SearchRepeat(
            CopyModeSearchRepeat::OppositeDirection,
        )),
        'o' => copy_mode_terminal_action(TerminalCopyModeAction::ToggleSelectionEnd),
        'q' => copy_mode_terminal_action(TerminalCopyModeAction::Cancel),
        'v' => copy_mode_terminal_action(TerminalCopyModeAction::ToggleSelection),
        'V' => copy_mode_terminal_action(TerminalCopyModeAction::SelectLine),
        'w' => copy_mode_motion(TerminalCopyModeMotion::NextWord),
        'y' => copy_mode_terminal_action(TerminalCopyModeAction::CopySelectionAndCancel),
        _ => None,
    }
}

fn copy_mode_motion(motion: TerminalCopyModeMotion) -> Option<CopyModeKeyAction> {
    copy_mode_terminal_action(TerminalCopyModeAction::Move(motion))
}
