use super::super::super::*;
use super::support::*;

fn test_mouse_size() -> MouseEncoderSize {
    MouseEncoderSize {
        screen_width: 800,
        screen_height: 480,
        cell_width: 10,
        cell_height: 20,
        padding_top: 0,
        padding_bottom: 0,
        padding_right: 0,
        padding_left: 0,
    }
}

fn unit_mouse_size() -> MouseEncoderSize {
    MouseEncoderSize {
        screen_width: 1_000,
        screen_height: 1_000,
        cell_width: 1,
        cell_height: 1,
        padding_top: 0,
        padding_bottom: 0,
        padding_right: 0,
        padding_left: 0,
    }
}

fn boundary_mouse_size() -> MouseEncoderSize {
    MouseEncoderSize {
        screen_width: 10,
        screen_height: 10,
        cell_width: 2,
        cell_height: 2,
        padding_top: 0,
        padding_bottom: 0,
        padding_right: 0,
        padding_left: 0,
    }
}

fn mouse_input(
    size: MouseEncoderSize,
    action: MouseAction,
    button: Option<MouseButton>,
    x: f32,
    y: f32,
) -> MouseInput {
    MouseInput {
        action,
        button,
        mods: KeyMods::default(),
        x,
        y,
        size,
    }
}

fn mouse_input_with_mods(
    size: MouseEncoderSize,
    action: MouseAction,
    button: Option<MouseButton>,
    x: f32,
    y: f32,
    mods: KeyMods,
) -> MouseInput {
    MouseInput {
        action,
        button,
        mods,
        x,
        y,
        size,
    }
}

fn test_mouse_input(
    action: MouseAction,
    button: Option<MouseButton>,
    x: f32,
    y: f32,
) -> MouseInput {
    mouse_input(test_mouse_size(), action, button, x, y)
}

fn unit_mouse_input(
    action: MouseAction,
    button: Option<MouseButton>,
    x: f32,
    y: f32,
) -> MouseInput {
    mouse_input(unit_mouse_size(), action, button, x, y)
}

fn assert_mouse_encode(
    engine: &mut TerminalEngine,
    out: &mut Vec<u8>,
    input: MouseInput,
    expected: &[u8],
) -> Result<()> {
    engine.encode_mouse_to_vec(input, out)?;
    assert_eq!(out, expected);
    Ok(())
}

fn assert_mouse_silent(
    engine: &mut TerminalEngine,
    out: &mut Vec<u8>,
    input: MouseInput,
) -> Result<()> {
    engine.encode_mouse_to_vec(input, out)?;
    assert!(out.is_empty());
    Ok(())
}

#[test]
fn mouse_encoder_inherits_sgr_press_release_and_motion_policy() -> Result<()> {
    let mut engine = test_terminal_engine()?;
    let mut out = Vec::new();
    let size = test_mouse_size();

    engine.encode_mouse_to_vec(
        MouseInput {
            action: MouseAction::Press,
            button: Some(MouseButton::Left),
            mods: KeyMods::default(),
            x: 0.0,
            y: 0.0,
            size,
        },
        &mut out,
    )?;
    assert!(out.is_empty());

    engine.write_vt(b"\x1b[?1000h\x1b[?1006h");
    engine.encode_mouse_to_vec(
        MouseInput {
            action: MouseAction::Press,
            button: Some(MouseButton::Left),
            mods: KeyMods::default(),
            x: 0.0,
            y: 0.0,
            size,
        },
        &mut out,
    )?;
    assert_eq!(out, b"\x1b[<0;1;1M");

    engine.encode_mouse_to_vec(
        MouseInput {
            action: MouseAction::Release,
            button: Some(MouseButton::Left),
            mods: KeyMods::default(),
            x: 0.0,
            y: 0.0,
            size,
        },
        &mut out,
    )?;
    assert_eq!(out, b"\x1b[<0;1;1m");

    engine.encode_mouse_to_vec(
        MouseInput {
            action: MouseAction::Motion,
            button: Some(MouseButton::Left),
            mods: KeyMods::default(),
            x: 10.0,
            y: 20.0,
            size,
        },
        &mut out,
    )?;
    assert!(out.is_empty());

    Ok(())
}

#[test]
fn mouse_encoder_supports_format_and_wheel_cases() -> Result<()> {
    let mut out = Vec::new();

    for (mode, input, expected) in [
        (
            b"\x1b[?9h".as_slice(),
            mouse_input_with_mods(
                test_mouse_size(),
                MouseAction::Press,
                Some(MouseButton::Left),
                0.0,
                0.0,
                KeyMods {
                    shift: true,
                    alt: true,
                    ctrl: true,
                    ..Default::default()
                },
            ),
            [0x1b, b'[', b'M', 32, 33, 33].as_slice(),
        ),
        (
            b"\x1b[?1003h\x1b[?1006h".as_slice(),
            test_mouse_input(MouseAction::Press, Some(MouseButton::Four), 0.0, 0.0),
            b"\x1b[<64;1;1M".as_slice(),
        ),
        (
            b"\x1b[?1003h\x1b[?1006h".as_slice(),
            test_mouse_input(MouseAction::Press, Some(MouseButton::Five), 0.0, 0.0),
            b"\x1b[<65;1;1M".as_slice(),
        ),
        (
            b"\x1b[?1003h\x1b[?1006h".as_slice(),
            test_mouse_input(MouseAction::Press, Some(MouseButton::Six), 0.0, 0.0),
            b"\x1b[<66;1;1M".as_slice(),
        ),
        (
            b"\x1b[?1003h\x1b[?1006h".as_slice(),
            test_mouse_input(MouseAction::Press, Some(MouseButton::Seven), 0.0, 0.0),
            b"\x1b[<67;1;1M".as_slice(),
        ),
        (
            b"\x1b[?1003h\x1b[?1006h".as_slice(),
            test_mouse_input(MouseAction::Press, Some(MouseButton::Left), 10.0, 20.0),
            b"\x1b[<0;2;2M".as_slice(),
        ),
        (
            b"\x1b[?1003h\x1b[?1016h".as_slice(),
            test_mouse_input(MouseAction::Press, Some(MouseButton::Left), 10.0, 20.0),
            b"\x1b[<0;10;20M".as_slice(),
        ),
        (
            b"\x1b[?1003h\x1b[?1016h".as_slice(),
            test_mouse_input(MouseAction::Release, Some(MouseButton::Right), 10.0, 20.0),
            b"\x1b[<2;10;20m".as_slice(),
        ),
    ] {
        let mut engine = test_terminal_engine()?;
        engine.write_vt(mode);
        assert_mouse_encode(&mut engine, &mut out, input, expected)?;
    }
    Ok(())
}

#[test]
fn mouse_encoder_rejects_unsupported_format_inputs() -> Result<()> {
    let mut out = Vec::new();

    for (mode, input) in [
        (
            b"\x1b[?9h".as_slice(),
            test_mouse_input(MouseAction::Release, Some(MouseButton::Left), 0.0, 0.0),
        ),
        (
            b"\x1b[?1003h\x1b[?1006h".as_slice(),
            test_mouse_input(MouseAction::Press, Some(MouseButton::Ten), 1.0, 1.0),
        ),
    ] {
        let mut engine = test_terminal_engine()?;
        engine.write_vt(mode);
        assert_mouse_silent(&mut engine, &mut out, input)?;
    }
    Ok(())
}

#[test]
fn mouse_encoder_reports_motion_after_initial_press_and_clamps_negative_cells() -> Result<()> {
    let size = test_mouse_size();
    let mut out = Vec::new();
    let mut engine = test_terminal_engine()?;

    engine.write_vt(b"\x1b[?1003h\x1b[?1006h");
    assert_mouse_encode(
        &mut engine,
        &mut out,
        mouse_input(
            size,
            MouseAction::Press,
            Some(MouseButton::Left),
            10.0,
            20.0,
        ),
        b"\x1b[<0;2;2M",
    )?;
    assert_mouse_encode(
        &mut engine,
        &mut out,
        mouse_input(
            size,
            MouseAction::Motion,
            Some(MouseButton::Left),
            -1.0,
            -1.0,
        ),
        b"\x1b[<32;1;1M",
    )
}

#[test]
fn mouse_encoder_supports_reporting_mode_cases() -> Result<()> {
    let mut out = Vec::new();

    for (mode, input, expected) in [
        (
            b"\x1b[?9h".as_slice(),
            unit_mouse_input(MouseAction::Press, Some(MouseButton::Left), 0.0, 0.0),
            [0x1b, b'[', b'M', 32, 33, 33].as_slice(),
        ),
        (
            b"\x1b[?9h".as_slice(),
            unit_mouse_input(MouseAction::Press, Some(MouseButton::Middle), 0.0, 0.0),
            [0x1b, b'[', b'M', 33, 33, 33].as_slice(),
        ),
        (
            b"\x1b[?9h".as_slice(),
            unit_mouse_input(MouseAction::Press, Some(MouseButton::Right), 0.0, 0.0),
            [0x1b, b'[', b'M', 34, 33, 33].as_slice(),
        ),
        (
            b"\x1b[?1000h\x1b[?1006h".as_slice(),
            unit_mouse_input(MouseAction::Press, Some(MouseButton::Left), 0.0, 0.0),
            b"\x1b[<0;1;1M".as_slice(),
        ),
        (
            b"\x1b[?1000h\x1b[?1006h".as_slice(),
            unit_mouse_input(MouseAction::Release, Some(MouseButton::Left), 0.0, 0.0),
            b"\x1b[<0;1;1m".as_slice(),
        ),
        (
            b"\x1b[?1002h\x1b[?1006h".as_slice(),
            unit_mouse_input(MouseAction::Motion, Some(MouseButton::Left), 1.0, 2.0),
            b"\x1b[<32;2;3M".as_slice(),
        ),
        (
            b"\x1b[?1003h\x1b[?1006h".as_slice(),
            mouse_input(unit_mouse_size(), MouseAction::Motion, None, 1.0, 2.0),
            b"\x1b[<35;2;3M".as_slice(),
        ),
    ] {
        let mut engine = test_terminal_engine()?;
        engine.write_vt(mode);
        assert_mouse_encode(&mut engine, &mut out, input, expected)?;
    }
    Ok(())
}

#[test]
fn mouse_encoder_suppresses_unreported_events() -> Result<()> {
    let mut out = Vec::new();

    for (mode, input) in [
        (
            b"".as_slice(),
            unit_mouse_input(MouseAction::Press, Some(MouseButton::Left), 0.0, 0.0),
        ),
        (
            b"".as_slice(),
            unit_mouse_input(MouseAction::Release, Some(MouseButton::Left), 0.0, 0.0),
        ),
        (
            b"".as_slice(),
            unit_mouse_input(MouseAction::Motion, Some(MouseButton::Left), 0.0, 0.0),
        ),
        (
            b"\x1b[?9h".as_slice(),
            unit_mouse_input(MouseAction::Release, Some(MouseButton::Left), 0.0, 0.0),
        ),
        (
            b"\x1b[?9h".as_slice(),
            unit_mouse_input(MouseAction::Motion, Some(MouseButton::Left), 0.0, 0.0),
        ),
        (
            b"\x1b[?9h".as_slice(),
            unit_mouse_input(MouseAction::Press, Some(MouseButton::Four), 0.0, 0.0),
        ),
        (
            b"\x1b[?9h".as_slice(),
            mouse_input(unit_mouse_size(), MouseAction::Press, None, 0.0, 0.0),
        ),
        (
            b"\x1b[?1000h\x1b[?1006h".as_slice(),
            unit_mouse_input(MouseAction::Motion, Some(MouseButton::Left), 1.0, 2.0),
        ),
        (
            b"\x1b[?1002h\x1b[?1006h".as_slice(),
            mouse_input(unit_mouse_size(), MouseAction::Motion, None, 1.0, 2.0),
        ),
    ] {
        let mut engine = test_terminal_engine()?;
        engine.write_vt(mode);
        assert_mouse_silent(&mut engine, &mut out, input)?;
    }
    Ok(())
}

#[test]
fn mouse_encoder_inherits_cell_motion_dedup_except_sgr_pixels() -> Result<()> {
    let size = test_mouse_size();
    let mut engine = test_terminal_engine()?;
    let mut out = Vec::new();

    engine.write_vt(b"\x1b[?1003h\x1b[?1006h");
    engine.encode_mouse_to_vec(
        MouseInput {
            action: MouseAction::Motion,
            button: Some(MouseButton::Left),
            mods: KeyMods::default(),
            x: 50.0,
            y: 120.0,
            size,
        },
        &mut out,
    )?;
    assert_eq!(out, b"\x1b[<32;6;7M");

    engine.encode_mouse_to_vec(
        MouseInput {
            action: MouseAction::Motion,
            button: Some(MouseButton::Left),
            mods: KeyMods::default(),
            x: 50.0,
            y: 120.0,
            size,
        },
        &mut out,
    )?;
    assert!(out.is_empty());

    engine.write_vt(b"\x1b[?1016h");
    engine.encode_mouse_to_vec(
        MouseInput {
            action: MouseAction::Motion,
            button: Some(MouseButton::Left),
            mods: KeyMods::default(),
            x: 50.0,
            y: 120.0,
            size,
        },
        &mut out,
    )?;
    assert_eq!(out, b"\x1b[<32;50;120M");

    Ok(())
}

#[test]
fn mouse_encoder_supports_urxvt_utf8_and_x10_limit_cases() -> Result<()> {
    let size = unit_mouse_size();
    let mut out = Vec::new();

    let mut urxvt = test_terminal_engine()?;
    urxvt.write_vt(b"\x1b[?1003h\x1b[?1015h");
    urxvt.encode_mouse_to_vec(
        MouseInput {
            action: MouseAction::Press,
            button: Some(MouseButton::Left),
            mods: KeyMods {
                shift: true,
                alt: true,
                ctrl: true,
                ..Default::default()
            },
            x: 2.0,
            y: 3.0,
            size,
        },
        &mut out,
    )?;
    assert_eq!(out, b"\x1b[60;3;4M");

    let mut utf8 = test_terminal_engine()?;
    utf8.write_vt(b"\x1b[?1003h\x1b[?1005h");
    utf8.encode_mouse_to_vec(
        MouseInput {
            action: MouseAction::Press,
            button: Some(MouseButton::Left),
            mods: KeyMods::default(),
            x: 300.0,
            y: 400.0,
            size,
        },
        &mut out,
    )?;
    let mut expected = vec![0x1b, b'[', b'M', 32];
    let mut encoded = [0; 4];
    expected.extend_from_slice(
        char::from_u32(333)
            .unwrap()
            .encode_utf8(&mut encoded)
            .as_bytes(),
    );
    expected.extend_from_slice(
        char::from_u32(433)
            .unwrap()
            .encode_utf8(&mut encoded)
            .as_bytes(),
    );
    assert_eq!(out, expected);

    let mut x10 = test_terminal_engine()?;
    x10.write_vt(b"\x1b[?9h");
    x10.encode_mouse_to_vec(
        MouseInput {
            action: MouseAction::Press,
            button: Some(MouseButton::Left),
            mods: KeyMods::default(),
            x: 223.0,
            y: 0.0,
            size,
        },
        &mut out,
    )?;
    assert!(out.is_empty());

    Ok(())
}

#[test]
fn mouse_encoder_ports_release_identity_and_boundary_cases() -> Result<()> {
    let size = unit_mouse_size();
    let mut out = Vec::new();

    let mut sgr = test_terminal_engine()?;
    sgr.write_vt(b"\x1b[?1003h\x1b[?1006h");
    sgr.encode_mouse_to_vec(
        MouseInput {
            action: MouseAction::Release,
            button: Some(MouseButton::Right),
            mods: KeyMods::default(),
            x: 4.0,
            y: 5.0,
            size,
        },
        &mut out,
    )?;
    assert_eq!(out, b"\x1b[<2;5;6m");

    let mut urxvt = test_terminal_engine()?;
    urxvt.write_vt(b"\x1b[?1003h\x1b[?1015h");
    urxvt.encode_mouse_to_vec(
        MouseInput {
            action: MouseAction::Release,
            button: Some(MouseButton::Right),
            mods: KeyMods::default(),
            x: 2.0,
            y: 3.0,
            size,
        },
        &mut out,
    )?;
    assert_eq!(out, b"\x1b[35;3;4M");

    let mut boundary = test_terminal_engine()?;
    boundary.write_vt(b"\x1b[?1003h\x1b[?1006h");
    boundary.encode_mouse_to_vec(
        MouseInput {
            action: MouseAction::Press,
            button: Some(MouseButton::Left),
            mods: KeyMods::default(),
            x: 10.0,
            y: 10.0,
            size: boundary_mouse_size(),
        },
        &mut out,
    )?;
    assert_eq!(out, b"\x1b[<0;5;5M");

    Ok(())
}
