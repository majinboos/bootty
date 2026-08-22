use bootty_surface::geometry::{MouseSurfaceMetrics, RoundedPadding};
use bootty_terminal::terminal_input_model::{KeyMods, MouseEncoderSize, TerminalKey};
use libghostty_vt::key;
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use proptest_derive::Arbitrary;

#[derive(Arbitrary, Debug)]
struct MouseMetricsCase {
    screen_width: u32,
    screen_height: u32,
    cell_width: u32,
    cell_height: u32,
    padding: [u32; 4],
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

proptest! {
    /// Surface dimensions and padding pass through unchanged while zero-sized cells are clamped
    /// to the encoder's minimum of one pixel.
    #[test]
    fn mouse_encoder_size_preserves_metrics(case in any::<MouseMetricsCase>()) {
        let encoded = MouseEncoderSize::from(MouseSurfaceMetrics {
            screen_width: case.screen_width,
            screen_height: case.screen_height,
            cell_width: case.cell_width,
            cell_height: case.cell_height,
            padding: RoundedPadding {
                top: case.padding[0],
                right: case.padding[1],
                bottom: case.padding[2],
                left: case.padding[3],
            },
        });

        prop_assert_eq!(encoded.screen_width, case.screen_width);
        prop_assert_eq!(encoded.screen_height, case.screen_height);
        prop_assert_eq!(encoded.cell_width, case.cell_width.max(1));
        prop_assert_eq!(encoded.cell_height, case.cell_height.max(1));
        prop_assert_eq!(
            [encoded.padding_top, encoded.padding_right, encoded.padding_bottom, encoded.padding_left],
            case.padding,
        );
    }
}
