use bootty_winit::{
    direct_input::{ModifierSideState, direct_key_input_from_winit_code},
    input::{InputSnapshot, TerminalInputCommand, terminal_input_commands},
    input_binding::{BindingAction, BindingKey, BindingMods, BindingTrigger, parse_binding},
    input_binding_set::BindingSet,
    terminal::{KeyInput, KeyMods, TerminalKey},
};
use eframe::egui;
use winit::keyboard::{KeyCode, ModifiersState};

#[test]
fn binding_parser_preserves_modifiers_flags_and_action_payload() {
    let binding = parse_binding("unconsumed:ctrl+KeyA=text:=hello").expect("binding parses");

    assert_eq!(
        binding.trigger,
        BindingTrigger {
            mods: BindingMods {
                ctrl: true,
                ..BindingMods::default()
            },
            key: BindingKey::Physical(TerminalKey::A),
        }
    );
    assert!(!binding.flags.consumed);
    assert_eq!(binding.action, BindingAction::Text("=hello".to_owned()));
}

#[test]
fn binding_set_prefers_a_side_specific_binding() {
    let mut bindings = BindingSet::default();
    bindings
        .parse_and_put("alt+KeyA=text:any")
        .expect("generic binding parses");
    bindings
        .parse_and_put("right_alt+KeyA=text:right")
        .expect("sided binding parses");

    let binding = bindings
        .get_event(KeyInput {
            key: TerminalKey::A,
            mods: KeyMods {
                alt: true,
                right_alt: true,
                ..KeyMods::default()
            },
            repeat: false,
            utf8: None,
            unshifted: None,
        })
        .expect("binding matches");

    assert_eq!(binding.action, BindingAction::Text("right".to_owned()));
}

#[test]
fn egui_ctrl_key_event_becomes_one_terminal_key_command() {
    let modifiers = egui::Modifiers {
        ctrl: true,
        ..egui::Modifiers::default()
    };
    let commands = terminal_input_commands(InputSnapshot {
        events: vec![egui::Event::Key {
            key: egui::Key::C,
            physical_key: Some(egui::Key::C),
            pressed: true,
            repeat: false,
            modifiers,
        }],
        modifiers,
        modifier_sides: ModifierSideState::default(),
        hover_pos: None,
        pressed_mouse_button: None,
        surface: None,
        mouse_exclusion: None,
        view: bootty_winit::geometry::ViewTransform::IDENTITY,
    });

    let [TerminalInputCommand::Key(input)] = commands.as_slice() else {
        panic!("expected one terminal key command");
    };
    assert_eq!(input.key, TerminalKey::C);
    assert!(input.mods.ctrl);
}

#[test]
fn direct_keypad_input_keeps_its_physical_identity() {
    let direct = direct_key_input_from_winit_code(
        KeyCode::Numpad1,
        ModifiersState::ALT,
        ModifierSideState::default(),
        false,
    )
    .expect("keypad input maps");

    assert_eq!(direct.input().key, TerminalKey::Numpad1);
    assert!(direct.input().mods.alt);
    assert_eq!(direct.input().utf8, Some("1"));
}
