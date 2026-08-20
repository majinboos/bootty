use super::{super::*, terminal_engine::test_terminal_engine};

fn terminal_key_input(
    key: TerminalKey,
    mods: KeyMods,
    utf8: Option<&'static str>,
    unshifted: Option<char>,
) -> KeyInput {
    KeyInput {
        key,
        mods,
        repeat: false,
        utf8,
        unshifted,
    }
}

fn assert_engine_key(
    engine: &mut TerminalEngine,
    out: &mut Vec<u8>,
    input: KeyInput,
    expected: &[u8],
) -> Result<()> {
    engine.encode_key_to_vec(input, out)?;
    assert_eq!(out, expected);
    Ok(())
}

#[derive(Clone, Copy)]
struct KeyEncodeCase<'a> {
    action: key::Action,
    key: key::Key,
    mods: key::Mods,
    consumed_mods: key::Mods,
    composing: bool,
    utf8: Option<&'a str>,
    unshifted: Option<char>,
    expected: &'a [u8],
}

impl<'a> KeyEncodeCase<'a> {
    fn press(key: key::Key, expected: &'a [u8]) -> Self {
        Self {
            action: key::Action::Press,
            key,
            mods: key::Mods::empty(),
            consumed_mods: key::Mods::empty(),
            composing: false,
            utf8: None,
            unshifted: None,
            expected,
        }
    }

    fn mods(mut self, mods: key::Mods) -> Self {
        self.mods = mods;
        self
    }

    fn consumed_mods(mut self, consumed_mods: key::Mods) -> Self {
        self.consumed_mods = consumed_mods;
        self
    }

    fn action(mut self, action: key::Action) -> Self {
        self.action = action;
        self
    }

    fn composing(mut self) -> Self {
        self.composing = true;
        self
    }

    fn utf8(mut self, utf8: &'a str) -> Self {
        self.utf8 = Some(utf8);
        self
    }

    fn unshifted(mut self, unshifted: char) -> Self {
        self.unshifted = Some(unshifted);
        self
    }
}

fn encode_key_case(
    case: KeyEncodeCase<'_>,
    configure: impl FnOnce(&mut key::Encoder),
) -> Result<Vec<u8>> {
    let mut encoder = key::Encoder::new()?;
    let mut event = key::Event::new()?;
    let mut out = Vec::new();

    configure(&mut encoder);
    event
        .set_action(case.action)
        .set_key(case.key)
        .set_mods(case.mods)
        .set_consumed_mods(case.consumed_mods)
        .set_composing(case.composing)
        .set_utf8(case.utf8);
    if let Some(unshifted) = case.unshifted {
        event.set_unshifted_codepoint(unshifted);
    }
    encoder.encode_to_vec(&event, &mut out)?;
    Ok(out)
}

fn encode_with_kitty_flags(case: KeyEncodeCase<'_>, flags: key::KittyKeyFlags) -> Result<Vec<u8>> {
    encode_key_case(case, |encoder| {
        encoder.set_kitty_flags(flags);
    })
}

fn encode_legacy_case(case: KeyEncodeCase<'_>) -> Result<Vec<u8>> {
    let terminal = Terminal::new(TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: 0,
    })?;
    encode_key_case(case, |encoder| {
        encoder
            .set_options_from_terminal(&terminal)
            .set_alt_esc_prefix(true)
            .set_macos_option_as_alt(key::OptionAsAlt::True);
    })
}

#[test]
fn key_encoder_supports_legacy_core_cases() -> Result<()> {
    let mut engine = test_terminal_engine()?;
    let mut out = Vec::new();

    assert_engine_key(
        &mut engine,
        &mut out,
        terminal_key_input(
            TerminalKey::C,
            KeyMods {
                ctrl: true,
                ..Default::default()
            },
            Some("c"),
            Some('c'),
        ),
        b"\x03",
    )?;

    assert_engine_key(
        &mut engine,
        &mut out,
        terminal_key_input(
            TerminalKey::D,
            KeyMods {
                ctrl: true,
                ..Default::default()
            },
            Some("d"),
            Some('d'),
        ),
        b"\x04",
    )?;

    assert_engine_key(
        &mut engine,
        &mut out,
        terminal_key_input(
            TerminalKey::B,
            KeyMods {
                alt: true,
                ..Default::default()
            },
            Some("b"),
            Some('b'),
        ),
        b"\x1bb",
    )?;

    assert_engine_key(
        &mut engine,
        &mut out,
        terminal_key_input(
            TerminalKey::Q,
            KeyMods {
                alt: true,
                shift: true,
                ..Default::default()
            },
            Some("Q"),
            Some('q'),
        ),
        b"\x1bQ",
    )?;

    assert_engine_key(
        &mut engine,
        &mut out,
        terminal_key_input(
            TerminalKey::C,
            KeyMods {
                ctrl: true,
                alt: true,
                ..Default::default()
            },
            Some("c"),
            Some('c'),
        ),
        b"\x1b\x03",
    )?;

    assert_engine_key(
        &mut engine,
        &mut out,
        terminal_key_input(
            TerminalKey::Space,
            KeyMods {
                ctrl: true,
                ..Default::default()
            },
            Some(" "),
            Some(' '),
        ),
        b"\x00",
    )?;

    assert_engine_key(
        &mut engine,
        &mut out,
        terminal_key_input(
            TerminalKey::Minus,
            KeyMods {
                ctrl: true,
                shift: true,
                ..Default::default()
            },
            Some("_"),
            Some('-'),
        ),
        b"\x1f",
    )?;

    assert_engine_key(
        &mut engine,
        &mut out,
        terminal_key_input(TerminalKey::Backspace, KeyMods::default(), None, None),
        b"\x7f",
    )?;

    engine.write_vt(b"\x1b[?67h");
    assert_engine_key(
        &mut engine,
        &mut out,
        terminal_key_input(TerminalKey::Backspace, KeyMods::default(), None, None),
        b"\x08",
    )?;

    assert_engine_key(
        &mut engine,
        &mut out,
        terminal_key_input(
            TerminalKey::Backspace,
            KeyMods {
                ctrl: true,
                ..Default::default()
            },
            None,
            None,
        ),
        b"\x7f",
    )?;

    assert_engine_key(
        &mut engine,
        &mut out,
        terminal_key_input(
            TerminalKey::ArrowUp,
            KeyMods {
                shift: true,
                ..Default::default()
            },
            None,
            None,
        ),
        b"\x1b[1;2A",
    )?;

    for (key, expected) in [
        (TerminalKey::F1, b"\x1b[1;5P".as_slice()),
        (TerminalKey::F2, b"\x1b[1;5Q".as_slice()),
        (TerminalKey::F3, b"\x1b[13;5~".as_slice()),
        (TerminalKey::F4, b"\x1b[1;5S".as_slice()),
        (TerminalKey::F5, b"\x1b[15;5~".as_slice()),
    ] {
        assert_engine_key(
            &mut engine,
            &mut out,
            terminal_key_input(
                key,
                KeyMods {
                    ctrl: true,
                    ..Default::default()
                },
                None,
                None,
            ),
            expected,
        )?;
    }

    assert_engine_key(
        &mut engine,
        &mut out,
        terminal_key_input(
            TerminalKey::Tab,
            KeyMods {
                shift: true,
                ..Default::default()
            },
            None,
            None,
        ),
        b"\x1b[Z",
    )?;

    Ok(())
}

#[test]
fn key_encoder_ports_kitty_protocol_compatibility_batch() -> Result<()> {
    let disambiguate = key::KittyKeyFlags::DISAMBIGUATE;
    let report_alternates =
        key::KittyKeyFlags::DISAMBIGUATE | key::KittyKeyFlags::REPORT_ALTERNATES;
    let all = key::KittyKeyFlags::ALL;

    for case in [
        KeyEncodeCase::press(key::Key::A, b"abcd").utf8("abcd"),
        KeyEncodeCase::press(key::Key::A, b"a")
            .action(key::Action::Repeat)
            .utf8("a"),
        KeyEncodeCase::press(key::Key::Enter, b"\r"),
        KeyEncodeCase::press(key::Key::Backspace, b"\x7f"),
        KeyEncodeCase::press(key::Key::Tab, b"\t"),
        KeyEncodeCase::press(key::Key::Backspace, b"\x1b[127;2u").mods(key::Mods::SHIFT),
        KeyEncodeCase::press(key::Key::Enter, b"\x1b[13;2u").mods(key::Mods::SHIFT),
        KeyEncodeCase::press(key::Key::Tab, b"\x1b[9;2u").mods(key::Mods::SHIFT),
        KeyEncodeCase::press(key::Key::Delete, b"\x1b[3~").utf8("\x7f"),
        KeyEncodeCase::press(key::Key::A, b"")
            .mods(key::Mods::SHIFT)
            .composing(),
        KeyEncodeCase::press(key::Key::ArrowUp, b"\x1b[A").utf8("\u{1e}"),
    ] {
        assert_eq!(encode_with_kitty_flags(case, disambiguate)?, case.expected);
    }

    let shift_a_alternate = KeyEncodeCase::press(key::Key::A, b"\x1b[97:65;2u")
        .mods(key::Mods::SHIFT)
        .utf8("A")
        .unshifted('a');
    assert_eq!(
        encode_with_kitty_flags(shift_a_alternate, report_alternates)?,
        shift_a_alternate.expected
    );

    for case in [
        KeyEncodeCase::press(key::Key::Enter, b"\x1b[13;1:3u").action(key::Action::Release),
        KeyEncodeCase::press(key::Key::Backspace, b"\x1b[127;1:3u").action(key::Action::Release),
        KeyEncodeCase::press(key::Key::Tab, b"\x1b[9;1:3u").action(key::Action::Release),
        KeyEncodeCase::press(key::Key::Enter, b"\x1b[13u"),
        KeyEncodeCase::press(key::Key::ControlLeft, b"\x1b[57442;5u").mods(key::Mods::CTRL),
        KeyEncodeCase::press(key::Key::ControlLeft, b"\x1b[57442;5:3u")
            .mods(key::Mods::CTRL)
            .action(key::Action::Release),
        KeyEncodeCase::press(key::Key::ShiftLeft, b"\x1b[57441;2u").mods(key::Mods::SHIFT),
        KeyEncodeCase::press(key::Key::ShiftRight, b"\x1b[57447;2u")
            .mods(key::Mods::SHIFT | key::Mods::SHIFT_SIDE),
        KeyEncodeCase::press(key::Key::AltLeft, b"\x1b[57443;3u").mods(key::Mods::ALT),
        KeyEncodeCase::press(key::Key::AltRight, b"\x1b[57449;3u")
            .mods(key::Mods::ALT | key::Mods::ALT_SIDE),
        KeyEncodeCase::press(key::Key::ShiftLeft, b"\x1b[57441;2u")
            .mods(key::Mods::SHIFT)
            .composing(),
        KeyEncodeCase::press(key::Key::Unidentified, "û".as_bytes()).utf8("û"),
        KeyEncodeCase::press(key::Key::Semicolon, b"\x1b[59:58;2;58u")
            .mods(key::Mods::SHIFT)
            .utf8(":")
            .unshifted(';'),
        KeyEncodeCase::press(key::Key::Semicolon, "\x1b[1095::59;;1095u".as_bytes())
            .utf8("ч")
            .unshifted('ч'),
        KeyEncodeCase::press(key::Key::Semicolon, "\x1b[1095:1063:59;2;1063u".as_bytes())
            .mods(key::Mods::SHIFT)
            .utf8("Ч")
            .unshifted('ч'),
        KeyEncodeCase::press(key::Key::J, b"\x1b[106;5u")
            .mods(key::Mods::CTRL)
            .utf8("j")
            .unshifted('j'),
        KeyEncodeCase::press(key::Key::J, b"\x1b[106:74;2;74u")
            .mods(key::Mods::SHIFT)
            .utf8("J")
            .unshifted('j'),
        KeyEncodeCase::press(key::Key::J, b"\x1b[106:74;2:3u")
            .mods(key::Mods::SHIFT)
            .action(key::Action::Release)
            .utf8("J")
            .unshifted('j'),
        KeyEncodeCase::press(key::Key::Delete, b"\x1b[3~").utf8("\x7f"),
        KeyEncodeCase::press(key::Key::Enter, b"A")
            .utf8("A")
            .unshifted('\r'),
        KeyEncodeCase::press(key::Key::Backspace, b"")
            .utf8("A")
            .unshifted('\r'),
    ] {
        assert_eq!(encode_with_kitty_flags(case, all)?, case.expected);
    }

    Ok(())
}

#[test]
fn key_encoder_ports_kitty_alternate_and_associated_text_batch() -> Result<()> {
    let report_alternates =
        key::KittyKeyFlags::DISAMBIGUATE | key::KittyKeyFlags::REPORT_ALTERNATES;
    let all = key::KittyKeyFlags::ALL;

    let matching_unshifted = KeyEncodeCase::press(key::Key::A, b"\x1b[65::97;2u")
        .mods(key::Mods::SHIFT)
        .utf8("A")
        .unshifted('A');
    assert_eq!(
        encode_with_kitty_flags(matching_unshifted, report_alternates)?,
        matching_unshifted.expected
    );

    for case in [
        KeyEncodeCase::press(key::Key::J, b"\x1b[106;65;74u")
            .mods(key::Mods::CAPS_LOCK)
            .utf8("J")
            .unshifted('j'),
        KeyEncodeCase::press(key::Key::Semicolon, "\x1b[1095::59;65;1063u".as_bytes())
            .mods(key::Mods::CAPS_LOCK)
            .utf8("Ч")
            .unshifted('ч'),
        KeyEncodeCase::press(key::Key::BracketLeft, "\x1b[337::91;5:3u".as_bytes())
            .mods(key::Mods::CTRL)
            .action(key::Action::Release)
            .utf8("")
            .unshifted('ő'),
    ] {
        assert_eq!(encode_with_kitty_flags(case, all)?, case.expected);
    }

    #[cfg(target_os = "macos")]
    {
        let option_text = KeyEncodeCase::press(key::Key::W, "\x1b[119;3;8721u".as_bytes())
            .mods(key::Mods::ALT)
            .utf8("∑")
            .unshifted('w');
        assert_eq!(
            encode_key_case(option_text, |encoder| {
                encoder
                    .set_kitty_flags(all)
                    .set_macos_option_as_alt(key::OptionAsAlt::False);
            })?,
            option_text.expected
        );

        let alt_text = KeyEncodeCase::press(key::Key::W, b"\x1b[119;3u")
            .mods(key::Mods::ALT)
            .utf8("∑")
            .unshifted('w');
        assert_eq!(
            encode_key_case(alt_text, |encoder| {
                encoder
                    .set_kitty_flags(all)
                    .set_macos_option_as_alt(key::OptionAsAlt::True);
            })?,
            alt_text.expected
        );

        let text_without_alt = KeyEncodeCase::press(key::Key::W, "\x1b[119;;8721u".as_bytes())
            .utf8("∑")
            .unshifted('w');
        assert_eq!(
            encode_key_case(text_without_alt, |encoder| {
                encoder
                    .set_kitty_flags(all)
                    .set_macos_option_as_alt(key::OptionAsAlt::True);
            })?,
            text_without_alt.expected
        );
    }

    Ok(())
}

#[test]
fn key_encoder_ports_kitty_sequence_formatting_edges() -> Result<()> {
    let all = key::KittyKeyFlags::ALL;

    for case in [
        KeyEncodeCase::press(key::Key::Backspace, b"\x1b[127u"),
        KeyEncodeCase::press(key::Key::Backspace, b"\x1b[127;1:3u").action(key::Action::Release),
        KeyEncodeCase::press(key::Key::Backspace, b"\x1b[127;2u").mods(key::Mods::SHIFT),
        KeyEncodeCase::press(key::Key::ArrowUp, b"\x1b[1;1:1A"),
        KeyEncodeCase::press(key::Key::ArrowUp, b"\x1b[1;2:1A").mods(key::Mods::SHIFT),
        KeyEncodeCase::press(key::Key::ArrowUp, b"\x1b[1;2:3A")
            .mods(key::Mods::SHIFT)
            .action(key::Action::Release),
        KeyEncodeCase::press(key::Key::J, b"\x1b[106;5u")
            .mods(key::Mods::CTRL)
            .utf8("j")
            .unshifted('j'),
        KeyEncodeCase::press(key::Key::J, b"\x1b[106:74;2;74u")
            .mods(key::Mods::SHIFT)
            .utf8("J")
            .unshifted('j'),
        KeyEncodeCase::press(key::Key::J, b"\x1b[106:74;2:3u")
            .mods(key::Mods::SHIFT)
            .action(key::Action::Release)
            .utf8("J")
            .unshifted('j'),
    ] {
        assert_eq!(encode_with_kitty_flags(case, all)?, case.expected);
    }

    Ok(())
}

#[test]
fn key_encoder_ports_kitty_keypad_and_backspace_mode_cases() -> Result<()> {
    let all = key::KittyKeyFlags::ALL;

    let keypad_one = KeyEncodeCase::press(key::Key::Numpad1, b"\x1b[57400;;49u").utf8("1");
    assert_eq!(
        encode_with_kitty_flags(keypad_one, all)?,
        keypad_one.expected
    );

    let backspace = KeyEncodeCase::press(key::Key::Backspace, b"\x1b[127u");
    for backarrow_key_mode in [false, true] {
        assert_eq!(
            encode_key_case(backspace, |encoder| {
                encoder
                    .set_kitty_flags(all)
                    .set_backarrow_key_mode(backarrow_key_mode);
            })?,
            backspace.expected
        );
    }

    Ok(())
}

#[test]
fn key_encoder_ports_legacy_extended_compatibility_batch() -> Result<()> {
    for case in [
        KeyEncodeCase::press(key::Key::Enter, b"A")
            .utf8("A")
            .unshifted('\r'),
        KeyEncodeCase::press(key::Key::Escape, b"A")
            .utf8("A")
            .unshifted('\r'),
        KeyEncodeCase::press(key::Key::Backspace, b"")
            .utf8("A")
            .unshifted('\r'),
        KeyEncodeCase::press(key::Key::E, b"\x1be")
            .mods(key::Mods::ALT)
            .unshifted('e'),
        KeyEncodeCase::press(key::Key::F, "ф".as_bytes())
            .mods(key::Mods::ALT)
            .utf8("ф"),
        KeyEncodeCase::press(key::Key::I, b"\x1b[105;5u")
            .mods(key::Mods::CTRL)
            .utf8("i"),
        KeyEncodeCase::press(key::Key::M, b"\x1b[109;5u")
            .mods(key::Mods::CTRL)
            .utf8("m"),
        KeyEncodeCase::press(key::Key::BracketLeft, b"\x1b[91;5u")
            .mods(key::Mods::CTRL)
            .utf8("["),
        KeyEncodeCase::press(key::Key::Digit2, b"\x1b[64;5u")
            .mods(key::Mods::CTRL | key::Mods::SHIFT)
            .utf8("@")
            .unshifted('2'),
        KeyEncodeCase::press(key::Key::M, b"\x1b[109;6u")
            .mods(key::Mods::CTRL | key::Mods::SHIFT)
            .utf8("M")
            .unshifted('m'),
        KeyEncodeCase::press(key::Key::ArrowUp, b"\x1b[1;2A")
            .mods(key::Mods::SHIFT)
            .consumed_mods(key::Mods::SHIFT),
        KeyEncodeCase::press(key::Key::BracketLeft, "\x1b[337;5u".as_bytes())
            .mods(key::Mods::CTRL)
            .utf8("ő")
            .unshifted('ő'),
        KeyEncodeCase::press(key::Key::Backspace, b"\x7f")
            .utf8("\x7f")
            .unshifted('\u{8}'),
        KeyEncodeCase::press(key::Key::Tab, b"\x1b[Z")
            .mods(key::Mods::SHIFT | key::Mods::SHIFT_SIDE),
    ] {
        assert_eq!(encode_legacy_case(case)?, case.expected);
    }

    Ok(())
}

#[test]
fn key_encoder_ports_control_sequence_mapping() -> Result<()> {
    for case in [
        KeyEncodeCase::press(key::Key::Unidentified, b"\x03")
            .mods(key::Mods::CTRL)
            .utf8("c")
            .unshifted('c'),
        KeyEncodeCase::press(key::Key::Unidentified, b"\x03")
            .mods(key::Mods::CTRL | key::Mods::CTRL_SIDE)
            .utf8("c")
            .unshifted('c'),
        KeyEncodeCase::press(key::Key::Unidentified, b"\x1b\x03")
            .mods(key::Mods::ALT | key::Mods::CTRL)
            .utf8("c")
            .unshifted('c'),
        KeyEncodeCase::press(key::Key::Unidentified, b"c")
            .utf8("c")
            .unshifted('c'),
        KeyEncodeCase::press(key::Key::Unidentified, b"\x1f")
            .mods(key::Mods::CTRL | key::Mods::SHIFT)
            .utf8("_")
            .unshifted('-'),
        KeyEncodeCase::press(key::Key::Unidentified, b"\x03")
            .mods(key::Mods::CTRL | key::Mods::CAPS_LOCK)
            .utf8("C")
            .unshifted('c'),
        KeyEncodeCase::press(key::Key::C, b"\x03")
            .mods(key::Mods::CTRL)
            .utf8("с")
            .unshifted('с'),
        KeyEncodeCase::press(key::Key::C, b"\x1b[1089;6u")
            .mods(key::Mods::CTRL | key::Mods::SHIFT)
            .utf8("с")
            .unshifted('с'),
        KeyEncodeCase::press(key::Key::C, b"\x1b\x03")
            .mods(key::Mods::ALT | key::Mods::CTRL)
            .utf8("с")
            .unshifted('с'),
        KeyEncodeCase::press(key::Key::C, b"\x03")
            .mods(key::Mods::CTRL | key::Mods::CTRL_SIDE)
            .utf8("с")
            .unshifted('c'),
    ] {
        assert_eq!(encode_legacy_case(case)?, case.expected);
    }

    Ok(())
}

#[test]
fn key_encoder_ports_platform_modifier_and_backspace_text_cases() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        for case in [
            KeyEncodeCase::press(key::Key::C, b"\x1bc")
                .mods(key::Mods::ALT)
                .utf8("≈")
                .unshifted('c'),
            KeyEncodeCase::press(key::Key::Period, b"\x1b>")
                .mods(key::Mods::ALT | key::Mods::SHIFT)
                .utf8(">")
                .unshifted('.'),
            KeyEncodeCase::press(key::Key::B, b"")
                .mods(key::Mods::SUPER)
                .utf8("b"),
            KeyEncodeCase::press(key::Key::B, b"")
                .mods(key::Mods::SUPER | key::Mods::SHIFT)
                .utf8("B"),
        ] {
            assert_eq!(encode_legacy_case(case)?, case.expected);
        }
    }

    let del_backspace = KeyEncodeCase::press(key::Key::Backspace, b"\x7f")
        .utf8("\x7f")
        .unshifted('\u{8}');
    assert_eq!(
        encode_key_case(del_backspace, |encoder| {
            encoder.set_backarrow_key_mode(false);
        })?,
        del_backspace.expected
    );

    let decbkm_backspace = KeyEncodeCase::press(key::Key::Backspace, b"\x08")
        .utf8("\x7f")
        .unshifted('\u{8}');
    assert_eq!(
        encode_key_case(decbkm_backspace, |encoder| {
            encoder.set_backarrow_key_mode(true);
        })?,
        decbkm_backspace.expected
    );

    Ok(())
}

#[test]
fn key_encoder_ports_function_sequences() -> Result<()> {
    let mut engine = test_terminal_engine()?;
    let mut out = Vec::new();
    for (terminal_key, plain, ctrl) in [
        (
            TerminalKey::F1,
            b"\x1bOP".as_slice(),
            b"\x1b[1;5P".as_slice(),
        ),
        (
            TerminalKey::F2,
            b"\x1bOQ".as_slice(),
            b"\x1b[1;5Q".as_slice(),
        ),
        (
            TerminalKey::F3,
            b"\x1bOR".as_slice(),
            b"\x1b[13;5~".as_slice(),
        ),
        (
            TerminalKey::F4,
            b"\x1bOS".as_slice(),
            b"\x1b[1;5S".as_slice(),
        ),
        (
            TerminalKey::F5,
            b"\x1b[15~".as_slice(),
            b"\x1b[15;5~".as_slice(),
        ),
        (
            TerminalKey::F6,
            b"\x1b[17~".as_slice(),
            b"\x1b[17;5~".as_slice(),
        ),
        (
            TerminalKey::F7,
            b"\x1b[18~".as_slice(),
            b"\x1b[18;5~".as_slice(),
        ),
        (
            TerminalKey::F8,
            b"\x1b[19~".as_slice(),
            b"\x1b[19;5~".as_slice(),
        ),
        (
            TerminalKey::F9,
            b"\x1b[20~".as_slice(),
            b"\x1b[20;5~".as_slice(),
        ),
        (
            TerminalKey::F10,
            b"\x1b[21~".as_slice(),
            b"\x1b[21;5~".as_slice(),
        ),
        (
            TerminalKey::F11,
            b"\x1b[23~".as_slice(),
            b"\x1b[23;5~".as_slice(),
        ),
        (
            TerminalKey::F12,
            b"\x1b[24~".as_slice(),
            b"\x1b[24;5~".as_slice(),
        ),
    ] {
        assert_engine_key(
            &mut engine,
            &mut out,
            terminal_key_input(terminal_key, KeyMods::default(), None, None),
            plain,
        )?;
        assert_engine_key(
            &mut engine,
            &mut out,
            terminal_key_input(
                terminal_key,
                KeyMods {
                    ctrl: true,
                    ..Default::default()
                },
                None,
                None,
            ),
            ctrl,
        )?;
    }

    Ok(())
}

#[test]
fn key_encoder_ports_keypad_identity_and_application_sequences() -> Result<()> {
    let mut engine = test_terminal_engine()?;
    let mut out = Vec::new();

    assert_engine_key(
        &mut engine,
        &mut out,
        terminal_key_input(TerminalKey::NumpadEnter, KeyMods::default(), None, None),
        b"\r",
    )?;

    assert_engine_key(
        &mut engine,
        &mut out,
        terminal_key_input(
            TerminalKey::Numpad1,
            KeyMods::default(),
            Some("1"),
            Some('1'),
        ),
        b"1",
    )?;

    for (key, utf8, expected) in [
        (key::Key::Numpad1, Some("1"), b"\x1bOq".as_slice()),
        (key::Key::NumpadAdd, Some("+"), b"\x1bOk".as_slice()),
        (key::Key::NumpadEnter, None, b"\x1bOM".as_slice()),
    ] {
        let mut case = KeyEncodeCase::press(key, expected);
        if let Some(utf8) = utf8 {
            case = case.utf8(utf8);
        }
        let encoded = encode_key_case(case, |encoder| {
            encoder.set_keypad_key_application(true);
        })?;
        assert_eq!(encoded, expected);
    }

    let numlock_ignored = encode_key_case(
        KeyEncodeCase::press(key::Key::Numpad1, b"1")
            .mods(key::Mods::NUM_LOCK)
            .utf8("1"),
        |encoder| {
            encoder
                .set_keypad_key_application(true)
                .set_ignore_keypad_with_numlock(true);
        },
    )?;
    assert_eq!(numlock_ignored, b"1");

    Ok(())
}

#[test]
fn key_encoder_ports_modify_other_keys_terminal_state() -> Result<()> {
    let mut engine = test_terminal_engine()?;
    let mut out = Vec::new();

    engine.write_vt(b"\x1b[>4;2m");
    assert_engine_key(
        &mut engine,
        &mut out,
        terminal_key_input(
            TerminalKey::H,
            KeyMods {
                shift: true,
                ctrl: true,
                ..Default::default()
            },
            Some("H"),
            Some('h'),
        ),
        b"\x1b[27;6;72~",
    )?;

    assert_engine_key(
        &mut engine,
        &mut out,
        terminal_key_input(
            TerminalKey::Digit8,
            KeyMods {
                alt: true,
                ..Default::default()
            },
            Some("8"),
            Some('8'),
        ),
        b"\x1b[27;3;56~",
    )?;

    Ok(())
}

#[test]
fn key_encoder_adapter_ports_options_and_kitty_ctrl_release() -> Result<()> {
    let terminal = Terminal::new(TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: 0,
    })?;
    let mut encoder = key::Encoder::new()?;
    let mut event = key::Event::new()?;

    encoder
        .set_cursor_key_application(true)
        .set_keypad_key_application(true)
        .set_kitty_flags(key::KittyKeyFlags::DISAMBIGUATE | key::KittyKeyFlags::REPORT_EVENTS)
        .set_macos_option_as_alt(key::OptionAsAlt::Left)
        .set_options_from_terminal(&terminal);

    event
        .set_action(key::Action::Release)
        .set_key(key::Key::ControlLeft)
        .set_mods(key::Mods::CTRL);

    encoder
        .set_kitty_flags(key::KittyKeyFlags::ALL)
        .set_macos_option_as_alt(key::OptionAsAlt::True);

    let mut out = Vec::new();
    encoder.encode_to_vec(&event, &mut out)?;
    assert_eq!(out, b"\x1b[57442;5:3u");
    Ok(())
}
