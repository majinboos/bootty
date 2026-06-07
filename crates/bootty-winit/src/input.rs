use eframe::egui::{self, Pos2, Rect};

use crate::{
    geometry::TerminalSurface,
    input_keymap::{
        egui_key_utf8, is_control_key, key_mods_from_egui_modifiers, key_unshifted,
        mouse_input_from_surface, mouse_mods_from_egui_modifiers, mouse_wheel_button_from_delta_y,
    },
    modifier_remap::ModifierRemapSet,
    terminal::{KeyInput, MouseAction, MouseButton, MouseInput, TerminalKey},
};

#[derive(Clone, Debug)]
pub struct InputSnapshot {
    pub events: Vec<egui::Event>,
    pub modifiers: egui::Modifiers,
    pub hover_pos: Option<Pos2>,
    pub pressed_mouse_button: Option<MouseButton>,
    pub surface: Option<TerminalSurface>,
    pub mouse_exclusion: Option<Rect>,
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

pub fn terminal_input_commands(snapshot: InputSnapshot) -> Vec<TerminalInputCommand> {
    terminal_input_commands_with_modifier_remaps(snapshot, &ModifierRemapSet::default())
}

pub fn terminal_input_commands_with_modifier_remaps(
    snapshot: InputSnapshot,
    modifier_remaps: &ModifierRemapSet,
) -> Vec<TerminalInputCommand> {
    let mut commands = Vec::with_capacity(snapshot.events.len());
    let suppress_modified_text = snapshot.modifiers.alt
        || snapshot.modifiers.ctrl
        || snapshot.modifiers.command
        || snapshot.modifiers.mac_cmd
        || snapshot.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::Key {
                    pressed: true,
                    modifiers,
                    ..
                } if modifiers.alt || modifiers.ctrl || modifiers.command || modifiers.mac_cmd
            )
        });

    for event in snapshot.events {
        match event {
            egui::Event::Text(text) if !suppress_modified_text => {
                commands.push(TerminalInputCommand::Text(text));
            }
            egui::Event::Ime(egui::ImeEvent::Commit(text)) if !suppress_modified_text => {
                commands.push(TerminalInputCommand::Text(text));
            }
            egui::Event::Paste(text) => commands.push(TerminalInputCommand::Paste(text)),
            egui::Event::WindowFocused(focused) => {
                commands.push(TerminalInputCommand::Focus(focused));
            }
            egui::Event::PointerMoved(pos) => {
                if !mouse_excluded(pos, snapshot.mouse_exclusion)
                    && let Some(input) = mouse_input(
                        pos,
                        MouseAction::Motion,
                        snapshot.pressed_mouse_button,
                        snapshot.modifiers,
                        snapshot.surface,
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
                if !mouse_excluded(pos, snapshot.mouse_exclusion)
                    && let Some(button) = terminal_mouse_button(button)
                {
                    let action = if pressed {
                        MouseAction::Press
                    } else {
                        MouseAction::Release
                    };
                    if let Some(input) =
                        mouse_input(pos, action, Some(button), modifiers, snapshot.surface)
                    {
                        commands.push(TerminalInputCommand::Mouse(input));
                    }
                }
            }
            egui::Event::MouseWheel {
                delta, modifiers, ..
            } => {
                if let (Some(pos), Some(button)) =
                    (snapshot.hover_pos, terminal_mouse_wheel_button(delta.y))
                    && !mouse_excluded(pos, snapshot.mouse_exclusion)
                    && let Some(input) = mouse_input(
                        pos,
                        MouseAction::Press,
                        Some(button),
                        modifiers,
                        snapshot.surface,
                    )
                {
                    commands.push(TerminalInputCommand::MouseWheel {
                        input,
                        scroll_delta: mouse_wheel_scroll_delta(delta.y),
                    });
                }
            }
            egui::Event::Key {
                key,
                pressed: true,
                repeat,
                modifiers,
                ..
            } => {
                if let Some(term_key) = terminal_key(key) {
                    if !should_encode_key(term_key, modifiers) {
                        continue;
                    }
                    let mut input = KeyInput {
                        key: term_key,
                        mods: key_mods_from_egui_modifiers(modifiers),
                        repeat,
                        utf8: egui_key_utf8(term_key, modifiers.shift),
                        unshifted: key_unshifted(term_key),
                    };
                    input.mods = modifier_remaps.apply(input.mods);
                    commands.push(TerminalInputCommand::Key(input));
                }
            }
            _ => {}
        }
    }

    commands
}

pub fn pressed_mouse_button_from_egui(pointer: &egui::PointerState) -> Option<MouseButton> {
    if pointer.button_down(egui::PointerButton::Primary) {
        Some(MouseButton::Left)
    } else if pointer.button_down(egui::PointerButton::Middle) {
        Some(MouseButton::Middle)
    } else if pointer.button_down(egui::PointerButton::Secondary) {
        Some(MouseButton::Right)
    } else {
        None
    }
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

fn should_encode_key(key: TerminalKey, modifiers: egui::Modifiers) -> bool {
    is_control_key(key) || modifiers.ctrl || modifiers.alt
}

fn mouse_excluded(pos: Pos2, exclusion: Option<Rect>) -> bool {
    exclusion.is_some_and(|rect| rect.contains(pos))
}

fn mouse_input(
    pos: Pos2,
    action: MouseAction,
    button: Option<MouseButton>,
    modifiers: egui::Modifiers,
    surface: Option<TerminalSurface>,
) -> Option<MouseInput> {
    let surface = surface?;
    mouse_input_from_surface(
        pos,
        action,
        button,
        mouse_mods_from_egui_modifiers(modifiers),
        surface,
    )
}

fn terminal_mouse_wheel_button(delta_y: f32) -> Option<MouseButton> {
    mouse_wheel_button_from_delta_y(delta_y)
}

fn mouse_wheel_scroll_delta(delta_y: f32) -> isize {
    if delta_y > 0.0 { -3 } else { 3 }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{CellMetrics, TerminalSurface};
    use crate::input_binding::{BindingKey, BindingMods, BindingTrigger};
    use crate::modifier_remap::ModifierRemapSet;
    use crate::terminal::{KeyMods, MouseEncoderSize};
    use eframe::egui::{Rect, Vec2};
    use proptest::prelude::*;

    fn modifiers(ctrl: bool, alt: bool, command: bool) -> egui::Modifiers {
        egui::Modifiers {
            ctrl,
            alt,
            command,
            ..Default::default()
        }
    }

    fn commands_for(events: Vec<egui::Event>) -> Vec<TerminalInputCommand> {
        terminal_input_commands(InputSnapshot {
            events,
            modifiers: egui::Modifiers::default(),
            hover_pos: None,
            pressed_mouse_button: None,
            surface: None,
            mouse_exclusion: None,
        })
    }

    #[test]
    fn ctrl_c_is_routed_to_terminal_encoder() {
        let commands = commands_for(vec![egui::Event::Key {
            key: egui::Key::C,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: modifiers(true, false, false),
        }]);

        assert_eq!(
            commands,
            vec![TerminalInputCommand::Key(KeyInput {
                key: TerminalKey::C,
                mods: KeyMods {
                    ctrl: true,
                    ..Default::default()
                },
                repeat: false,
                utf8: Some("c"),
                unshifted: Some('c'),
            })]
        );
    }

    #[test]
    fn egui_host_key_input_matches_physical_binding_trigger() {
        let commands = commands_for(vec![egui::Event::Key {
            key: egui::Key::C,
            physical_key: Some(egui::Key::C),
            pressed: true,
            repeat: false,
            modifiers: modifiers(true, false, false),
        }]);

        let [TerminalInputCommand::Key(input)] = commands.as_slice() else {
            panic!("expected one terminal key command");
        };
        assert_eq!(
            BindingTrigger::from_key_input(*input),
            BindingTrigger {
                mods: BindingMods {
                    ctrl: true,
                    ..Default::default()
                },
                key: BindingKey::Physical(TerminalKey::C)
            }
        );
    }

    #[test]
    fn readline_alt_shortcuts_are_routed_to_terminal_encoder() {
        let commands = commands_for(vec![egui::Event::Key {
            key: egui::Key::B,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: modifiers(false, true, false),
        }]);

        assert_eq!(
            commands,
            vec![TerminalInputCommand::Key(KeyInput {
                key: TerminalKey::B,
                mods: KeyMods {
                    alt: true,
                    ..Default::default()
                },
                repeat: false,
                utf8: Some("b"),
                unshifted: Some('b'),
            })]
        );
    }

    #[test]
    fn unmodified_printable_text_is_not_double_encoded() {
        let commands = commands_for(vec![egui::Event::Key {
            key: egui::Key::A,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: modifiers(false, false, false),
        }]);

        assert!(commands.is_empty());
    }

    #[test]
    fn alt_modified_text_is_not_double_sent() {
        let commands = terminal_input_commands(InputSnapshot {
            events: vec![
                egui::Event::Key {
                    key: egui::Key::Num2,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: modifiers(false, true, false),
                },
                egui::Event::Text("¡".to_owned()),
            ],
            modifiers: egui::Modifiers::default(),
            hover_pos: None,
            pressed_mouse_button: None,
            surface: None,
            mouse_exclusion: None,
        });
        assert_eq!(
            commands,
            vec![TerminalInputCommand::Key(KeyInput {
                key: TerminalKey::Digit2,
                mods: KeyMods {
                    alt: true,
                    ..Default::default()
                },
                repeat: false,
                utf8: Some("2"),
                unshifted: Some('2'),
            })]
        );
    }

    #[test]
    fn mac_command_modified_text_is_not_sent_to_terminal() {
        let commands = terminal_input_commands(InputSnapshot {
            events: vec![
                egui::Event::Key {
                    key: egui::Key::N,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::MAC_CMD,
                },
                egui::Event::Text("~2".to_owned()),
                egui::Event::Text("2~".to_owned()),
            ],
            modifiers: egui::Modifiers::MAC_CMD,
            hover_pos: None,
            pressed_mouse_button: None,
            surface: None,
            mouse_exclusion: None,
        });
        assert!(commands.is_empty());
    }

    #[test]
    fn alt_shift_letters_keep_shifted_utf8_for_terminal_encoder() {
        let commands = commands_for(vec![egui::Event::Key {
            key: egui::Key::Q,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                alt: true,
                shift: true,
                ..Default::default()
            },
        }]);

        assert_eq!(
            commands,
            vec![TerminalInputCommand::Key(KeyInput {
                key: TerminalKey::Q,
                mods: KeyMods {
                    shift: true,
                    alt: true,
                    ..Default::default()
                },
                repeat: false,
                utf8: Some("Q"),
                unshifted: Some('q'),
            })]
        );
    }

    #[test]
    fn backspace_is_routed_to_terminal_encoder() {
        let commands = commands_for(vec![egui::Event::Key {
            key: egui::Key::Backspace,
            physical_key: Some(egui::Key::Backspace),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        }]);

        assert_eq!(
            commands,
            vec![TerminalInputCommand::Key(KeyInput {
                key: TerminalKey::Backspace,
                mods: KeyMods::default(),
                repeat: false,
                utf8: None,
                unshifted: None,
            })]
        );
    }

    #[test]
    fn command_printable_shortcuts_are_reserved_for_platform_policy() {
        let commands = commands_for(vec![egui::Event::Key {
            key: egui::Key::C,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: modifiers(false, false, true),
        }]);

        assert!(commands.is_empty());
    }

    #[test]
    fn egui_host_applies_modifier_remaps_before_terminal_encoding() {
        let mut remaps = ModifierRemapSet::default();
        remaps.parse("alt=ctrl").unwrap();
        remaps.finalize();

        let commands = terminal_input_commands_with_modifier_remaps(
            InputSnapshot {
                events: vec![egui::Event::Key {
                    key: egui::Key::B,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: modifiers(false, true, false),
                }],
                modifiers: egui::Modifiers::default(),
                hover_pos: None,
                pressed_mouse_button: None,
                surface: None,
                mouse_exclusion: None,
            },
            &remaps,
        );

        assert_eq!(
            commands,
            vec![TerminalInputCommand::Key(KeyInput {
                key: TerminalKey::B,
                mods: KeyMods {
                    ctrl: true,
                    ..Default::default()
                },
                repeat: false,
                utf8: Some("b"),
                unshifted: Some('b'),
            })]
        );
    }

    #[test]
    fn text_paste_and_ime_are_distinct_commands() {
        let commands = commands_for(vec![
            egui::Event::Text("a".to_owned()),
            egui::Event::Ime(egui::ImeEvent::Commit("é".to_owned())),
            egui::Event::Paste("hello".to_owned()),
        ]);

        assert_eq!(
            commands,
            vec![
                TerminalInputCommand::Text("a".to_owned()),
                TerminalInputCommand::Text("é".to_owned()),
                TerminalInputCommand::Paste("hello".to_owned()),
            ]
        );
    }

    #[test]
    fn window_focus_events_are_routed_to_terminal_focus_encoder() {
        let commands = commands_for(vec![
            egui::Event::WindowFocused(true),
            egui::Event::WindowFocused(false),
        ]);

        assert_eq!(
            commands,
            vec![
                TerminalInputCommand::Focus(true),
                TerminalInputCommand::Focus(false),
            ]
        );
    }

    #[test]
    fn mouse_input_is_relative_to_terminal_rect() {
        let rect = Rect::from_min_max(Pos2::new(20.0, 40.0), Pos2::new(220.0, 140.0));
        let input = mouse_input(
            Pos2::new(35.0, 70.0),
            MouseAction::Press,
            Some(MouseButton::Left),
            modifiers(true, false, false),
            Some(TerminalSurface::for_rect(rect, CellMetrics::new(9.0, 22.0))),
        )
        .expect("inside terminal rect");

        assert_eq!(input.action, MouseAction::Press);
        assert_eq!(input.button, Some(MouseButton::Left));
        assert!(input.mods.ctrl);
        assert_eq!(input.x, 15.0);
        assert_eq!(input.y, 30.0);
        assert_eq!(input.size.screen_width, 200);
        assert_eq!(input.size.screen_height, 100);
        assert_eq!(input.size.cell_width, 9);
        assert_eq!(input.size.cell_height, 22);
        assert_eq!(input.size.padding_left, 0);
    }

    #[test]
    fn pointer_motion_preserves_pressed_button_for_button_tracking() {
        let rect = Rect::from_min_max(Pos2::new(20.0, 40.0), Pos2::new(220.0, 140.0));
        let commands = terminal_input_commands(InputSnapshot {
            events: vec![egui::Event::PointerMoved(Pos2::new(35.0, 70.0))],
            modifiers: egui::Modifiers::default(),
            hover_pos: Some(Pos2::new(35.0, 70.0)),
            pressed_mouse_button: Some(MouseButton::Left),
            surface: Some(TerminalSurface::for_rect(rect, CellMetrics::new(9.0, 22.0))),
            mouse_exclusion: None,
        });

        assert_eq!(
            commands,
            vec![TerminalInputCommand::Mouse(MouseInput {
                action: MouseAction::Motion,
                button: Some(MouseButton::Left),
                mods: KeyMods::default(),
                x: 15.0,
                y: 30.0,
                size: MouseEncoderSize {
                    screen_width: 200,
                    screen_height: 100,
                    cell_width: 9,
                    cell_height: 22,
                    padding_left: 0,
                    padding_top: 0,
                    padding_right: 0,
                    padding_bottom: 0,
                },
            })]
        );
    }

    #[test]
    fn mouse_wheel_events_map_to_terminal_buttons_at_hover_position() {
        let rect = Rect::from_min_max(Pos2::new(20.0, 40.0), Pos2::new(220.0, 140.0));
        let commands = terminal_input_commands(InputSnapshot {
            events: vec![
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: Vec2::new(0.0, 1.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: modifiers(false, true, false),
                },
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: Vec2::new(0.0, -1.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::default(),
                },
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: Vec2::ZERO,
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::default(),
                },
            ],
            modifiers: egui::Modifiers::default(),
            hover_pos: Some(Pos2::new(35.0, 70.0)),
            pressed_mouse_button: None,
            surface: Some(TerminalSurface::for_rect(rect, CellMetrics::new(9.0, 22.0))),
            mouse_exclusion: None,
        });

        let size = MouseEncoderSize {
            screen_width: 200,
            screen_height: 100,
            cell_width: 9,
            cell_height: 22,
            padding_left: 0,
            padding_top: 0,
            padding_right: 0,
            padding_bottom: 0,
        };

        assert_eq!(
            commands,
            vec![
                TerminalInputCommand::MouseWheel {
                    input: MouseInput {
                        action: MouseAction::Press,
                        button: Some(MouseButton::Four),
                        mods: KeyMods {
                            alt: true,
                            ..Default::default()
                        },
                        x: 15.0,
                        y: 30.0,
                        size,
                    },
                    scroll_delta: -3,
                },
                TerminalInputCommand::MouseWheel {
                    input: MouseInput {
                        action: MouseAction::Press,
                        button: Some(MouseButton::Five),
                        mods: KeyMods::default(),
                        x: 15.0,
                        y: 30.0,
                        size,
                    },
                    scroll_delta: 3,
                },
            ]
        );
    }

    #[test]
    fn mouse_events_inside_exclusion_rect_are_not_sent_to_terminal() {
        let terminal_rect = Rect::from_min_max(Pos2::new(20.0, 40.0), Pos2::new(220.0, 140.0));
        let exclusion = Rect::from_min_max(Pos2::new(204.0, 40.0), Pos2::new(220.0, 140.0));
        let commands = terminal_input_commands(InputSnapshot {
            events: vec![
                egui::Event::PointerButton {
                    pos: Pos2::new(210.0, 70.0),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                },
                egui::Event::PointerMoved(Pos2::new(210.0, 72.0)),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: Vec2::new(0.0, 1.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::default(),
                },
            ],
            modifiers: egui::Modifiers::default(),
            hover_pos: Some(Pos2::new(210.0, 72.0)),
            pressed_mouse_button: Some(MouseButton::Left),
            surface: Some(TerminalSurface::for_rect(
                terminal_rect,
                CellMetrics::new(9.0, 22.0),
            )),
            mouse_exclusion: Some(exclusion),
        });

        assert!(commands.is_empty());
    }

    #[test]
    fn mouse_input_outside_terminal_rect_is_ignored() {
        let rect = Rect::from_min_max(Pos2::new(20.0, 40.0), Pos2::new(220.0, 140.0));
        assert!(
            mouse_input(
                Pos2::new(10.0, 70.0),
                MouseAction::Motion,
                None,
                egui::Modifiers::default(),
                Some(TerminalSurface::for_rect(rect, CellMetrics::new(9.0, 22.0))),
            )
            .is_none()
        );
    }

    proptest! {
        #[test]
        fn property_pointer_inside_surface_maps_to_non_negative_relative_position(
            x in 0_u32..400,
            y in 0_u32..300,
        ) {
            let rect = Rect::from_min_size(Pos2::new(20.0, 40.0), Vec2::new(400.0, 300.0));
            let pos = Pos2::new(20.0 + x as f32, 40.0 + y as f32);
            let input = mouse_input(
                pos,
                MouseAction::Motion,
                None,
                egui::Modifiers::default(),
                Some(TerminalSurface::for_rect(rect, CellMetrics::new(9.0, 22.0))),
            ).expect("generated point is inside");

            prop_assert!(input.x >= 0.0);
            prop_assert!(input.y >= 0.0);
            prop_assert!(input.x <= 400.0);
            prop_assert!(input.y <= 300.0);
        }
    }
}
