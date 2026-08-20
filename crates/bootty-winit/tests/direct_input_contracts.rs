use bootty_winit::{
    direct_input::{
        ModifierSideState, direct_key_input_from_winit_code,
        direct_key_input_from_winit_code_with_remaps, suppress_egui_events_for_direct_input,
    },
    input_binding::{BindingKey, BindingMods, BindingTrigger},
    modifier_remap::ModifierRemapSet,
    terminal::{KeyMods, TerminalKey},
};
use eframe::egui;
use winit::{
    event::ElementState,
    keyboard::{KeyCode, ModifiersState},
};

fn key_event(key: egui::Key, repeat: bool) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat,
        modifiers: egui::Modifiers::default(),
    }
}

#[test]
fn keypad_input_preserves_physical_identity_and_text() {
    let direct = direct_key_input_from_winit_code(
        KeyCode::Numpad1,
        ModifiersState::ALT,
        ModifierSideState::default(),
        false,
    )
    .expect("keypad input maps");

    assert_eq!(direct.input().key, TerminalKey::Numpad1);
    assert_eq!(
        direct.input().mods,
        KeyMods {
            alt: true,
            ..KeyMods::default()
        }
    );
    assert_eq!(direct.input().utf8, Some("1"));
    assert_eq!(direct.input().unshifted, Some('1'));
    assert_eq!(
        BindingTrigger::from_key_input(direct.input()),
        BindingTrigger {
            mods: BindingMods {
                alt: true,
                ..BindingMods::default()
            },
            key: BindingKey::Physical(TerminalKey::Numpad1),
        }
    );
}

#[test]
fn direct_input_suppresses_only_matching_collapsed_egui_events() {
    let first = direct_key_input_from_winit_code(
        KeyCode::Numpad1,
        ModifiersState::empty(),
        ModifierSideState::default(),
        false,
    )
    .expect("keypad input maps");
    let repeated = direct_key_input_from_winit_code(
        KeyCode::Numpad1,
        ModifiersState::empty(),
        ModifierSideState::default(),
        true,
    )
    .expect("keypad repeat maps");
    let mut events = vec![
        key_event(egui::Key::Num1, false),
        egui::Event::Text("1".to_owned()),
        egui::Event::PointerMoved(egui::pos2(1.0, 2.0)),
        key_event(egui::Key::Num1, true),
        egui::Event::Text("1".to_owned()),
        egui::Event::Text("1".to_owned()),
        key_event(egui::Key::A, false),
    ];

    suppress_egui_events_for_direct_input(&mut events, &[first, repeated]);

    assert_eq!(
        events,
        vec![
            egui::Event::PointerMoved(egui::pos2(1.0, 2.0)),
            egui::Event::Text("1".to_owned()),
            key_event(egui::Key::A, false),
        ]
    );
}

#[test]
fn main_row_digits_and_standalone_modifiers_remain_on_the_normal_path() {
    assert!(
        direct_key_input_from_winit_code(
            KeyCode::Digit1,
            ModifiersState::empty(),
            ModifierSideState::default(),
            false,
        )
        .is_none()
    );
    assert!(
        direct_key_input_from_winit_code(
            KeyCode::ShiftLeft,
            ModifiersState::SHIFT,
            ModifierSideState::default(),
            false,
        )
        .is_none()
    );
}

#[test]
fn side_state_drops_sides_when_the_aggregate_modifier_is_released() {
    let mut sides = ModifierSideState {
        left_shift: true,
        right_alt: true,
        left_ctrl: true,
        left_command: true,
        right_command: true,
        ..ModifierSideState::default()
    };

    sides.retain_active_modifiers(ModifiersState::ALT);

    assert_eq!(
        sides,
        ModifierSideState {
            right_alt: true,
            ..ModifierSideState::default()
        }
    );
}

#[test]
fn command_modified_keys_keep_stale_and_combined_modifier_state() {
    let direct = direct_key_input_from_winit_code(
        KeyCode::KeyB,
        ModifiersState::empty(),
        ModifierSideState {
            left_command: true,
            ..ModifierSideState::default()
        },
        false,
    )
    .expect("stale aggregate state still maps");
    assert_eq!(direct.input().key, TerminalKey::B);
    assert_eq!(
        direct.input().mods,
        KeyMods {
            command: true,
            ..KeyMods::default()
        }
    );

    let combined = direct_key_input_from_winit_code(
        KeyCode::KeyX,
        ModifiersState::SUPER | ModifiersState::ALT,
        ModifierSideState::default(),
        false,
    )
    .expect("combined command input maps");
    assert_eq!(
        BindingTrigger::from_key_input(combined.input()).format_entry(),
        "cmd+alt+KeyX"
    );
}

#[test]
fn right_shift_tab_preserves_modifier_side() {
    let mut sides = ModifierSideState::default();
    sides.update_key(KeyCode::ShiftRight, ElementState::Pressed);

    let direct =
        direct_key_input_from_winit_code(KeyCode::Tab, ModifiersState::SHIFT, sides, false)
            .expect("right shift tab maps");

    assert_eq!(direct.input().key, TerminalKey::Tab);
    assert_eq!(
        direct.input().mods,
        KeyMods {
            shift: true,
            right_shift: true,
            ..KeyMods::default()
        }
    );
}

#[test]
fn direct_input_applies_the_configured_modifier_remap() {
    let mut remaps = ModifierRemapSet::default();
    remaps.parse("left_alt=right_ctrl").expect("remap parses");
    remaps.finalize();

    let direct = direct_key_input_from_winit_code_with_remaps(
        KeyCode::Numpad1,
        ModifiersState::ALT,
        ModifierSideState::default(),
        false,
        &remaps,
    )
    .expect("keypad input maps");

    assert_eq!(
        direct.input().mods,
        KeyMods {
            ctrl: true,
            right_ctrl: true,
            ..KeyMods::default()
        }
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_ctrl_c_uses_direct_terminal_input() {
    let direct = direct_key_input_from_winit_code(
        KeyCode::KeyC,
        ModifiersState::CONTROL,
        ModifierSideState::default(),
        false,
    )
    .expect("ctrl+c maps");

    assert_eq!(direct.input().key, TerminalKey::C);
    assert!(direct.input().mods.ctrl);
}
