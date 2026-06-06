use super::super::super::*;
use super::support::*;

#[test]
fn terminal_engine_answers_size_queries_from_current_geometry() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;

    for (query, expected) in [
        (b"\x1b[14t".as_slice(), b"\x1b[4;480;800t".as_slice()),
        (b"\x1b[16t".as_slice(), b"\x1b[6;20;10t".as_slice()),
        (b"\x1b[18t".as_slice(), b"\x1b[8;24;80t".as_slice()),
    ] {
        engine.write_vt(query);
        assert_eq!(take_pty_output(&output), expected);
    }
    Ok(())
}

#[test]
fn terminal_engine_reports_resizes_when_window_ops_are_enabled() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;

    engine.resize(TerminalGeometry {
        cols: 80,
        rows: 24,
        cell_width: 9,
        cell_height: 18,
    })?;

    engine.write_vt(b"\x1b[14t\x1b[16t\x1b[18t");
    assert_eq!(
        take_pty_output(&output),
        b"\x1b[4;432;720t\x1b[6;18;9t\x1b[8;24;80t",
    );

    engine.write_vt(b"\x1b[?2048h");
    engine.resize(TerminalGeometry {
        cols: 100,
        rows: 40,
        cell_width: 9,
        cell_height: 18,
    })?;
    assert_eq!(take_pty_output(&output), b"\x1b[48;40;100;720;900t");

    Ok(())
}

type CapturedDeviceAttributesEngine = (
    TerminalEngine,
    Arc<Mutex<Vec<u8>>>,
    Arc<Mutex<DeviceAttributes>>,
);

fn captured_device_attributes_engine() -> Result<CapturedDeviceAttributesEngine> {
    let (mut engine, output) = captured_pty_engine()?;
    let attributes = Arc::new(Mutex::new(default_device_attributes()));
    let callback_attributes = attributes.clone();
    engine
        .terminal
        .on_device_attributes(move |_terminal| callback_attributes.lock().ok().map(|a| *a))?;
    Ok((engine, output, attributes))
}

#[test]
fn terminal_engine_reports_primary_device_attributes() -> Result<()> {
    let (mut engine, output, attributes) = captured_device_attributes_engine()?;

    for (primary, expected) in [
        (
            PrimaryDeviceAttributes::new(
                ConformanceLevel::VT220,
                [DeviceAttributeFeature::ANSI_COLOR],
            ),
            b"\x1b[?62;22c".as_slice(),
        ),
        (
            PrimaryDeviceAttributes::new(
                ConformanceLevel::VT220,
                [
                    DeviceAttributeFeature::ANSI_COLOR,
                    DeviceAttributeFeature::CLIPBOARD,
                ],
            ),
            b"\x1b[?62;22;52c".as_slice(),
        ),
        (
            PrimaryDeviceAttributes::new(
                ConformanceLevel::VT420,
                [
                    DeviceAttributeFeature::COLUMNS_132,
                    DeviceAttributeFeature::SELECTIVE_ERASE,
                    DeviceAttributeFeature::ANSI_COLOR,
                ],
            ),
            b"\x1b[?64;1;6;22c".as_slice(),
        ),
        (
            PrimaryDeviceAttributes::new(ConformanceLevel::VT100, []),
            b"\x1b[?1c".as_slice(),
        ),
    ] {
        attributes.lock().expect("attributes lock").primary = primary;
        engine.write_vt(b"\x1b[c");
        assert_eq!(take_pty_output(&output), expected);
    }
    Ok(())
}

#[test]
fn terminal_engine_reports_secondary_and_tertiary_device_attributes() -> Result<()> {
    let (mut engine, output, attributes) = captured_device_attributes_engine()?;

    engine.write_vt(b"\x1b[>c");
    assert_eq!(take_pty_output(&output), b"\x1b[>1;0;0c");

    engine.write_vt(b"\x1b[=c");
    assert_eq!(take_pty_output(&output), b"\x1bP!|00000000\x1b\\");

    attributes.lock().expect("attributes lock").tertiary = TertiaryDeviceAttributes {
        unit_id: 0xAABBCCDD,
    };
    engine.write_vt(b"\x1b[=c");
    assert_eq!(take_pty_output(&output), b"\x1bP!|AABBCCDD\x1b\\");

    Ok(())
}

#[test]
fn terminal_engine_updates_mode_state_without_pty_output() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;
    assert!(engine.terminal.mode(Mode::WRAPAROUND)?);
    engine.terminal.set_mode(Mode::DECCKM, true)?;
    assert!(engine.terminal.mode(Mode::DECCKM)?);
    engine.terminal.set_mode(Mode::DECCKM, false)?;
    assert!(!engine.terminal.mode(Mode::DECCKM)?);
    engine.terminal.set_mode(Mode::INSERT, true)?;
    assert!(engine.terminal.mode(Mode::INSERT)?);
    assert_eq!(take_pty_output(&output), b"");
    Ok(())
}

#[test]
fn terminal_engine_reports_mode_query_status() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;

    for (input, expected) in [
        (b"\x1b[?7$p".as_slice(), b"\x1b[?7;1$y".as_slice()),
        (b"\x1b[?7l\x1b[?7$p".as_slice(), b"\x1b[?7;2$y".as_slice()),
        (b"\x1b[?9999$p".as_slice(), b"\x1b[?9999;0$y".as_slice()),
        (
            b"\x1b[?7l\x1b[?7s\x1b[?7h\x1b[?7r\x1b[?7$p".as_slice(),
            b"\x1b[?7;2$y".as_slice(),
        ),
    ] {
        engine.write_vt(input);
        assert_eq!(take_pty_output(&output), expected);
    }
    Ok(())
}

#[test]
fn terminal_engine_answers_status_queries() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;

    for (input, expected) in [
        (b"\x1b[5n".as_slice(), b"\x1b[0n".as_slice()),
        (b"\x1b[6n".as_slice(), b"\x1b[1;1R".as_slice()),
        (
            b"\x1b[5;20r\x1b[?6h\x1b[3;5H\x1b[6n".as_slice(),
            b"\x1b[3;5R".as_slice(),
        ),
        (b"\x1b[19t".as_slice(), b"".as_slice()),
    ] {
        engine.write_vt(input);
        assert_eq!(take_pty_output(&output), expected);
    }
    Ok(())
}

#[test]
fn terminal_engine_reports_color_scheme_when_requested() -> Result<()> {
    for (scheme, expected) in [
        (None, b"".as_slice()),
        (
            Some(libghostty_vt::terminal::ColorScheme::Dark),
            b"\x1b[?997;1n".as_slice(),
        ),
        (
            Some(libghostty_vt::terminal::ColorScheme::Light),
            b"\x1b[?997;2n".as_slice(),
        ),
    ] {
        let (mut engine, output) = captured_pty_engine()?;
        engine.write_vt(b"\x1b[?2031h");
        if let Some(scheme) = scheme {
            engine
                .terminal
                .on_color_scheme(move |_terminal| Some(scheme))?;
        }
        engine.write_vt(b"\x1b[?996n");
        assert_eq!(take_pty_output(&output), expected);
    }
    Ok(())
}

#[test]
fn terminal_engine_ignores_invalid_titles_and_malformed_sequences() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;
    let title_changes = Arc::new(Mutex::new(0_u32));
    let callback_title_changes = title_changes.clone();
    engine.terminal.on_title_changed(move |_terminal| {
        *callback_title_changes.lock().expect("title lock") += 1;
    })?;

    engine.write_vt(b"\x1b]2;outer\x1b\\");
    assert_eq!(engine.terminal.title()?, "outer");
    engine.write_vt(b"\x1b]2;bad\xc0\x1b\\");
    assert_eq!(engine.terminal.title()?, "outer");
    assert_eq!(*title_changes.lock().expect("title lock"), 1);

    let too_many_params = format!("\x1b[{}C", ["1"; 40].join(";"));
    engine.write_vt(too_many_params.as_bytes());
    engine.write_vt(b"\x1b[111111111111111111111111111111111111111111111111111111111111111111C");
    engine.write_vt(b"\x1bP6;;;;;;;;;;;;;;;;;;pignored\x1b\\");
    assert_eq!(take_pty_output(&output), b"");

    Ok(())
}

#[test]
fn terminal_engine_handles_query_side_effects_and_keyboard_reports() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;

    let bell_count = Arc::new(Mutex::new(0_u32));
    let callback_bells = bell_count.clone();
    engine.terminal.on_bell(move |_terminal| {
        *callback_bells.lock().expect("bell lock") += 1;
    })?;
    engine
        .terminal
        .on_enquiry(|_terminal| Some("bootty-enquiry"))?;
    engine
        .terminal
        .on_xtversion(|_terminal| Some("Bootty 1.0"))?;

    engine.write_vt(b"\x07\x05\x1b[>q");
    assert_eq!(*bell_count.lock().expect("bell lock"), 1);
    assert_eq!(
        take_pty_output(&output),
        b"bootty-enquiry\x1bP>|Bootty 1.0\x1b\\",
    );

    engine.write_vt(b"\x1b[?u");
    assert_eq!(take_pty_output(&output), b"\x1b[?0u");
    engine.write_vt(b"\x1b[>1u\x1b[?u");
    assert_eq!(take_pty_output(&output), b"\x1b[?1u");

    engine.write_vt(b"\x1b]2;My Title\x1b\\");
    assert_eq!(take_pty_output(&output), b"");
    engine.write_vt(b"\x1b[21t");
    assert_eq!(take_pty_output(&output), b"\x1b]lMy Title\x1b\\");

    Ok(())
}
