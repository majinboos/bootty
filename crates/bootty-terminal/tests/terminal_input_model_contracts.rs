use bootty_surface::geometry::{MouseSurfaceMetrics, RoundedPadding};
use bootty_terminal::terminal_input_model::{KeyMods, MouseEncoderSize, TerminalKey};
use libghostty_vt::key;

#[test]
fn mouse_surface_metrics_map_nonzero_fields_and_clamp_zero_cells() {
    let size = MouseEncoderSize::from(MouseSurfaceMetrics {
        screen_width: 800,
        screen_height: 480,
        cell_width: 0,
        cell_height: 0,
        padding: RoundedPadding {
            top: 7,
            right: 11,
            bottom: 13,
            left: 17,
        },
    });
    assert_eq!(size.cell_width, 1);
    assert_eq!(size.cell_height, 1);
    assert_eq!(
        (
            size.padding_top,
            size.padding_right,
            size.padding_bottom,
            size.padding_left
        ),
        (7, 11, 13, 17)
    );
}

#[test]
fn key_mods_convert_lock_and_right_side_flags() {
    let mods: key::Mods = KeyMods {
        shift: true,
        alt: true,
        ctrl: true,
        command: true,
        caps_lock: true,
        num_lock: true,
        right_shift: true,
        right_alt: true,
        right_ctrl: true,
        right_command: true,
    }
    .into();
    assert!(mods.contains(key::Mods::SHIFT | key::Mods::ALT | key::Mods::CTRL | key::Mods::SUPER));
    assert!(mods.contains(
        key::Mods::CAPS_LOCK
            | key::Mods::NUM_LOCK
            | key::Mods::SHIFT_SIDE
            | key::Mods::ALT_SIDE
            | key::Mods::CTRL_SIDE
            | key::Mods::SUPER_SIDE
    ));
}

#[test]
fn terminal_keypad_keys_convert_to_libghostty_keypad_keys() {
    assert_eq!(key::Key::from(TerminalKey::NumpadAdd), key::Key::NumpadAdd);
    assert_eq!(
        key::Key::from(TerminalKey::NumpadEnter),
        key::Key::NumpadEnter
    );
}
