use eframe::egui::{self, Pos2, Rect};

use crate::{
    geometry::{TerminalSurface, ViewTransform},
    input_keymap::{
        ModifierSideState, egui_key_utf8, is_control_key, key_mods_from_egui_modifiers,
        key_unshifted, mouse_input_from_surface_clamped_with_view,
        mouse_input_from_surface_with_view, mouse_mods_from_egui_modifiers,
        mouse_wheel_button_from_delta_y,
    },
    modifier_remap::ModifierRemapSet,
    terminal::{KeyInput, MacosOptionAsAlt, MouseAction, MouseButton, MouseInput, TerminalKey},
};

#[derive(Clone, Debug)]
pub struct InputSnapshot {
    pub events: Vec<egui::Event>,
    pub modifiers: egui::Modifiers,
    pub modifier_sides: ModifierSideState,
    pub hover_pos: Option<Pos2>,
    pub pressed_mouse_button: Option<MouseButton>,
    pub surface: Option<TerminalSurface>,
    pub mouse_exclusion: Option<Rect>,
    pub view: ViewTransform,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TerminalInputCommand {
    Text(String),
    Paste(String),
    Focus(bool),
    Key(KeyInput),
    Mouse(MouseInput),
    MouseWheel {
        input: MouseInput,
        scroll_delta: isize,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WheelScrollState {
    point_remainder_y: f32,
    line_remainder_y: f32,
}

pub fn terminal_input_commands(snapshot: InputSnapshot) -> Vec<TerminalInputCommand> {
    terminal_input_commands_with_modifier_remaps(snapshot, &ModifierRemapSet::default())
}

pub fn terminal_input_commands_with_modifier_remaps(
    snapshot: InputSnapshot,
    modifier_remaps: &ModifierRemapSet,
) -> Vec<TerminalInputCommand> {
    terminal_input_commands_with_options(snapshot, modifier_remaps, MacosOptionAsAlt::default())
}

pub fn terminal_input_commands_with_options(
    snapshot: InputSnapshot,
    modifier_remaps: &ModifierRemapSet,
    macos_option_as_alt: MacosOptionAsAlt,
) -> Vec<TerminalInputCommand> {
    let mut wheel_state = WheelScrollState::default();
    terminal_input_commands_with_wheel_state(
        snapshot,
        modifier_remaps,
        macos_option_as_alt,
        &mut wheel_state,
    )
}

pub fn terminal_input_commands_with_wheel_state(
    snapshot: InputSnapshot,
    modifier_remaps: &ModifierRemapSet,
    macos_option_as_alt: MacosOptionAsAlt,
    wheel_state: &mut WheelScrollState,
) -> Vec<TerminalInputCommand> {
    let mut commands = Vec::with_capacity(snapshot.events.len());
    let suppress_modified_text = std::iter::once(snapshot.modifiers)
        .chain(snapshot.events.iter().filter_map(|event| match event {
            egui::Event::Key {
                pressed: true,
                modifiers,
                ..
            } => Some(*modifiers),
            _ => None,
        }))
        .any(|modifiers| {
            text_modifiers_are_suppressed(modifiers, macos_option_as_alt, snapshot.modifier_sides)
        });

    // `pressed_mouse_button` is the pointer state after the whole frame, so a press landing in the
    // same frame as a move would tag that earlier move as a drag of a button the child has not seen
    // pressed yet. Nothing is held before this frame's first press; a frame that only releases
    // keeps the snapshot so an untracked release stays untracked.
    let opens_with_press = snapshot
        .events
        .iter()
        .find_map(|event| match event {
            egui::Event::PointerButton {
                button, pressed, ..
            } if terminal_mouse_button(*button).is_some() => Some(*pressed),
            _ => None,
        })
        .unwrap_or(false);
    let mut held = if opens_with_press {
        None
    } else {
        snapshot.pressed_mouse_button
    };

    for event in snapshot.events {
        match event {
            egui::Event::Text(text) | egui::Event::Ime(egui::ImeEvent::Commit(text))
                if !suppress_modified_text =>
            {
                commands.push(TerminalInputCommand::Text(text));
            }
            egui::Event::Paste(text) => commands.push(TerminalInputCommand::Paste(text)),
            egui::Event::WindowFocused(focused) => {
                commands.push(TerminalInputCommand::Focus(focused));
            }
            egui::Event::PointerMoved(pos) => {
                if !mouse_excluded(pos, snapshot.mouse_exclusion)
                    && let Some(input) = mouse_input_with_view(
                        pos,
                        MouseAction::Motion,
                        held,
                        snapshot.modifiers,
                        snapshot.surface,
                        snapshot.view,
                        false,
                    )
                {
                    commands.push(TerminalInputCommand::Mouse(input));
                }
            }
            egui::Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers,
            } => {
                let Some(button) = terminal_mouse_button(button) else {
                    continue;
                };
                let action = if pressed {
                    MouseAction::Press
                } else {
                    MouseAction::Release
                };
                let was_held = held;
                held = pressed.then_some(button);
                let input = if !pressed && was_held == Some(button) {
                    mouse_input_with_view(
                        pos,
                        action,
                        Some(button),
                        modifiers,
                        snapshot.surface,
                        snapshot.view,
                        true,
                    )
                } else if !mouse_excluded(pos, snapshot.mouse_exclusion) {
                    mouse_input_with_view(
                        pos,
                        action,
                        Some(button),
                        modifiers,
                        snapshot.surface,
                        snapshot.view,
                        false,
                    )
                } else {
                    None
                };
                if let Some(input) = input {
                    commands.push(TerminalInputCommand::Mouse(input));
                }
            }
            egui::Event::MouseWheel {
                unit,
                delta,
                modifiers,
                ..
            } => {
                if let (Some(pos), Some(button)) =
                    (snapshot.hover_pos, mouse_wheel_button_from_delta_y(delta.y))
                    && !mouse_excluded(pos, snapshot.mouse_exclusion)
                {
                    let scroll_delta =
                        mouse_wheel_scroll_delta(delta.y, unit, snapshot.surface, wheel_state);
                    if scroll_delta == 0 {
                        continue;
                    }
                    if let Some(input) = mouse_input_with_view(
                        pos,
                        MouseAction::Press,
                        Some(button),
                        modifiers,
                        snapshot.surface,
                        snapshot.view,
                        false,
                    ) {
                        commands.push(TerminalInputCommand::MouseWheel {
                            input,
                            scroll_delta,
                        });
                    }
                }
            }
            egui::Event::Key {
                key,
                pressed: true,
                repeat,
                modifiers,
                ..
            } => {
                let Some(term_key) = terminal_key(key) else {
                    continue;
                };
                if !should_encode_key(
                    term_key,
                    modifiers,
                    macos_option_as_alt,
                    snapshot.modifier_sides,
                ) {
                    continue;
                }
                let mut input = KeyInput {
                    key: term_key,
                    mods: key_mods_from_egui_modifiers(modifiers),
                    repeat,
                    utf8: egui_key_utf8(term_key, modifiers.shift),
                    unshifted: key_unshifted(term_key),
                };
                snapshot.modifier_sides.apply_to_key_input(&mut input);
                input.mods = modifier_remaps.apply(input.mods);
                commands.push(TerminalInputCommand::Key(input));
            }
            _ => {}
        }
    }

    commands
}

pub fn pressed_mouse_button_from_egui(pointer: &egui::PointerState) -> Option<MouseButton> {
    [
        egui::PointerButton::Primary,
        egui::PointerButton::Middle,
        egui::PointerButton::Secondary,
    ]
    .into_iter()
    .find(|button| pointer.button_down(*button))
    .and_then(terminal_mouse_button)
}

pub fn terminal_key(key: egui::Key) -> Option<TerminalKey> {
    match key {
        egui::Key::A => Some(TerminalKey::A),
        egui::Key::B => Some(TerminalKey::B),
        egui::Key::C => Some(TerminalKey::C),
        egui::Key::D => Some(TerminalKey::D),
        egui::Key::E => Some(TerminalKey::E),
        egui::Key::F => Some(TerminalKey::F),
        egui::Key::G => Some(TerminalKey::G),
        egui::Key::H => Some(TerminalKey::H),
        egui::Key::I => Some(TerminalKey::I),
        egui::Key::J => Some(TerminalKey::J),
        egui::Key::K => Some(TerminalKey::K),
        egui::Key::L => Some(TerminalKey::L),
        egui::Key::M => Some(TerminalKey::M),
        egui::Key::N => Some(TerminalKey::N),
        egui::Key::O => Some(TerminalKey::O),
        egui::Key::P => Some(TerminalKey::P),
        egui::Key::Q => Some(TerminalKey::Q),
        egui::Key::R => Some(TerminalKey::R),
        egui::Key::S => Some(TerminalKey::S),
        egui::Key::T => Some(TerminalKey::T),
        egui::Key::U => Some(TerminalKey::U),
        egui::Key::V => Some(TerminalKey::V),
        egui::Key::W => Some(TerminalKey::W),
        egui::Key::X => Some(TerminalKey::X),
        egui::Key::Y => Some(TerminalKey::Y),
        egui::Key::Z => Some(TerminalKey::Z),
        egui::Key::Num0 => Some(TerminalKey::Digit0),
        egui::Key::Num1 | egui::Key::Exclamationmark => Some(TerminalKey::Digit1),
        egui::Key::Num2 => Some(TerminalKey::Digit2),
        egui::Key::Num3 => Some(TerminalKey::Digit3),
        egui::Key::Num4 => Some(TerminalKey::Digit4),
        egui::Key::Num5 => Some(TerminalKey::Digit5),
        egui::Key::Num6 => Some(TerminalKey::Digit6),
        egui::Key::Num7 => Some(TerminalKey::Digit7),
        egui::Key::Num8 => Some(TerminalKey::Digit8),
        egui::Key::Num9 => Some(TerminalKey::Digit9),
        egui::Key::Space => Some(TerminalKey::Space),
        egui::Key::Backtick => Some(TerminalKey::Backquote),
        egui::Key::Backslash | egui::Key::Pipe => Some(TerminalKey::Backslash),
        egui::Key::OpenBracket | egui::Key::OpenCurlyBracket => Some(TerminalKey::BracketLeft),
        egui::Key::CloseBracket | egui::Key::CloseCurlyBracket => Some(TerminalKey::BracketRight),
        egui::Key::Comma => Some(TerminalKey::Comma),
        egui::Key::Minus => Some(TerminalKey::Minus),
        egui::Key::Period => Some(TerminalKey::Period),
        egui::Key::Plus | egui::Key::Equals => Some(TerminalKey::Equal),
        egui::Key::Semicolon | egui::Key::Colon => Some(TerminalKey::Semicolon),
        egui::Key::Quote => Some(TerminalKey::Quote),
        egui::Key::Slash | egui::Key::Questionmark => Some(TerminalKey::Slash),
        egui::Key::Enter => Some(TerminalKey::Enter),
        egui::Key::Tab => Some(TerminalKey::Tab),
        egui::Key::Backspace => Some(TerminalKey::Backspace),
        egui::Key::Escape => Some(TerminalKey::Escape),
        egui::Key::Insert => Some(TerminalKey::Insert),
        egui::Key::ArrowUp => Some(TerminalKey::ArrowUp),
        egui::Key::ArrowDown => Some(TerminalKey::ArrowDown),
        egui::Key::ArrowRight => Some(TerminalKey::ArrowRight),
        egui::Key::ArrowLeft => Some(TerminalKey::ArrowLeft),
        egui::Key::Delete => Some(TerminalKey::Delete),
        egui::Key::Home => Some(TerminalKey::Home),
        egui::Key::End => Some(TerminalKey::End),
        egui::Key::PageUp => Some(TerminalKey::PageUp),
        egui::Key::PageDown => Some(TerminalKey::PageDown),
        egui::Key::F1 => Some(TerminalKey::F1),
        egui::Key::F2 => Some(TerminalKey::F2),
        egui::Key::F3 => Some(TerminalKey::F3),
        egui::Key::F4 => Some(TerminalKey::F4),
        egui::Key::F5 => Some(TerminalKey::F5),
        egui::Key::F6 => Some(TerminalKey::F6),
        egui::Key::F7 => Some(TerminalKey::F7),
        egui::Key::F8 => Some(TerminalKey::F8),
        egui::Key::F9 => Some(TerminalKey::F9),
        egui::Key::F10 => Some(TerminalKey::F10),
        egui::Key::F11 => Some(TerminalKey::F11),
        egui::Key::F12 => Some(TerminalKey::F12),
        _ => None,
    }
}

fn should_encode_key(
    key: TerminalKey,
    modifiers: egui::Modifiers,
    macos_option_as_alt: MacosOptionAsAlt,
    modifier_sides: ModifierSideState,
) -> bool {
    is_control_key(key)
        || modifiers.ctrl
        || (modifiers.alt && option_alt_is_meta(macos_option_as_alt, modifier_sides))
}

fn text_modifiers_are_suppressed(
    modifiers: egui::Modifiers,
    macos_option_as_alt: MacosOptionAsAlt,
    modifier_sides: ModifierSideState,
) -> bool {
    modifiers.ctrl
        || modifiers.command
        || modifiers.mac_cmd
        || (modifiers.alt && option_alt_is_meta(macos_option_as_alt, modifier_sides))
}

fn option_alt_is_meta(
    macos_option_as_alt: MacosOptionAsAlt,
    modifier_sides: ModifierSideState,
) -> bool {
    match macos_option_as_alt {
        MacosOptionAsAlt::None => false,
        MacosOptionAsAlt::Both => true,
        MacosOptionAsAlt::Left => modifier_sides.left_alt || !modifier_sides.has_alt(),
        MacosOptionAsAlt::Right => modifier_sides.right_alt || !modifier_sides.has_alt(),
    }
}

fn mouse_excluded(pos: Pos2, exclusion: Option<Rect>) -> bool {
    exclusion.is_some_and(|rect| rect.contains(pos))
}

fn mouse_input_with_view(
    pos: Pos2,
    action: MouseAction,
    button: Option<MouseButton>,
    modifiers: egui::Modifiers,
    surface: Option<TerminalSurface>,
    view: ViewTransform,
    clamped: bool,
) -> Option<MouseInput> {
    let surface = surface?;
    let mods = mouse_mods_from_egui_modifiers(modifiers);
    if clamped {
        Some(mouse_input_from_surface_clamped_with_view(
            pos, action, button, mods, surface, view,
        ))
    } else {
        mouse_input_from_surface_with_view(pos, action, button, mods, surface, view)
    }
}

fn mouse_wheel_scroll_delta(
    delta_y: f32,
    unit: egui::MouseWheelUnit,
    surface: Option<TerminalSurface>,
    wheel_state: &mut WheelScrollState,
) -> isize {
    let (remainder, divisor) = match unit {
        egui::MouseWheelUnit::Point => {
            let cell_height = surface
                .map(|surface| surface.cell.height)
                .unwrap_or(crate::geometry::CellMetrics::default().height)
                .max(1.0);
            (&mut wheel_state.point_remainder_y, cell_height)
        }
        egui::MouseWheelUnit::Line => (&mut wheel_state.line_remainder_y, 1.0),
        egui::MouseWheelUnit::Page => {
            let rows = surface
                .map(|surface| isize::try_from(surface.geometry().rows).unwrap_or(isize::MAX))
                .unwrap_or(24);
            return if delta_y > 0.0 { -rows } else { rows };
        }
    };
    *remainder += delta_y;
    let whole = (*remainder / divisor).trunc();
    if whole == 0.0 {
        return 0;
    }
    *remainder -= whole * divisor;
    -(whole as isize)
}

fn terminal_mouse_button(button: egui::PointerButton) -> Option<MouseButton> {
    match button {
        egui::PointerButton::Primary => Some(MouseButton::Left),
        egui::PointerButton::Secondary => Some(MouseButton::Right),
        egui::PointerButton::Middle => Some(MouseButton::Middle),
        egui::PointerButton::Extra1 => Some(MouseButton::Four),
        egui::PointerButton::Extra2 => Some(MouseButton::Five),
    }
}
