use super::super::super::*;
use super::support::*;
use proptest::prelude::*;
use rstest::rstest;

#[test]
fn bracketed_paste_wraps_payload() -> Result<()> {
    let mut data = *b"hello\nworld";
    let mut buf = [0_u8; 64];
    let written = paste::encode(&mut data, true, &mut buf)?;

    assert_eq!(&buf[..written], b"\x1b[200~hello\nworld\x1b[201~");
    Ok(())
}

#[rstest]
#[case::plain_text("hello", true)]
#[case::trailing_newline("hello\n", false)]
#[case::embedded_newline("hello\nworld", false)]
#[case::bracketed_paste_end_marker("he\x1b[201~llo", false)]
fn paste_safety_classifies_control_sequences(#[case] input: &str, #[case] safe: bool) {
    assert_eq!(paste::is_safe(input), safe);
}

#[rstest]
#[case::plain_text("hello", b"hello".as_slice())]
#[case::line_feed_becomes_carriage_return("hello\nworld", b"hello\rworld".as_slice())]
#[case::crlf_becomes_two_carriage_returns("hello\r\nworld", b"hello\r\rworld".as_slice())]
#[case::control_byte_becomes_space("hel\x03lo", b"hel lo".as_slice())]
fn paste_encoder_normalizes_unbracketed_input(
    #[case] input: &str,
    #[case] expected: &[u8],
) -> Result<()> {
    let mut engine = test_terminal_engine()?;
    let mut out = Vec::new();
    engine.encode_paste_to_vec(input, &mut out)?;
    assert_eq!(out, expected);
    Ok(())
}

#[rstest]
#[case::embedded_escape_and_nul("hel\x1blo\x00world", b"\x1b[200~hel lo world\x1b[201~".as_slice())]
#[case::control_bytes("\x00\x08\x7f", b"\x1b[200~   \x1b[201~".as_slice())]
fn paste_encoder_wraps_bracketed_input(#[case] input: &str, #[case] expected: &[u8]) -> Result<()> {
    let mut engine = test_terminal_engine()?;
    let mut out = Vec::new();
    engine.write_vt(b"\x1b[?2004h");
    engine.encode_paste_to_vec(input, &mut out)?;
    assert_eq!(out, expected);
    Ok(())
}

#[test]
fn paste_encoder_reports_required_capacity_and_adapter_retries() -> Result<()> {
    let mut data = *b"hello";
    let mut small = [0_u8; 1];

    match paste::encode(&mut data, true, &mut small) {
        Err(libghostty_vt::Error::OutOfSpace { required }) => assert_eq!(required, 17),
        result => panic!("expected out-of-space from undersized paste buffer: {result:?}"),
    }

    let mut engine = test_terminal_engine()?;
    let mut out = Vec::new();
    engine.write_vt(b"\x1b[?2004h");
    engine.encode_paste_to_vec("hello", &mut out)?;
    assert_eq!(out, b"\x1b[200~hello\x1b[201~");

    Ok(())
}

proptest! {
    #[test]
    fn unbracketed_paste_never_emits_terminal_control_bytes(input in "\\PC*") {
        let mut engine = test_terminal_engine().expect("terminal engine");
        let mut out = Vec::new();

        engine
            .encode_paste_to_vec(&input, &mut out)
            .expect("encode paste");

        prop_assert!(
            out.iter().all(|byte| *byte >= b' ' || *byte == b'\r'),
            "unbracketed paste should only emit printable bytes or carriage returns: {out:?}"
        );
        prop_assert!(!out.contains(&b'\n'));
        prop_assert!(!out.contains(&b'\x1b'));
        prop_assert!(!out.contains(&b'\x7f'));
    }

    #[test]
    fn bracketed_paste_wraps_and_sanitizes_payload(input in "\\PC*") {
        let mut engine = test_terminal_engine().expect("terminal engine");
        let mut out = Vec::new();
        engine.write_vt(b"\x1b[?2004h");

        engine
            .encode_paste_to_vec(&input, &mut out)
            .expect("encode bracketed paste");

        prop_assert!(out.starts_with(b"\x1b[200~"));
        prop_assert!(out.ends_with(b"\x1b[201~"));
        let payload = &out[b"\x1b[200~".len()..out.len() - b"\x1b[201~".len()];
        prop_assert!(
            payload.iter().all(|byte| *byte >= b' ' || *byte == b'\r'),
            "bracketed paste payload should only emit printable bytes or carriage returns: {payload:?}"
        );
        prop_assert!(!payload.contains(&b'\n'));
        prop_assert!(!payload.contains(&b'\x1b'));
        prop_assert!(!payload.contains(&b'\x7f'));
    }
}
