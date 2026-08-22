use super::super::super::*;
use super::support::*;
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

#[rstest]
#[case::plain_text("hello", true)]
#[case::trailing_newline("hello\n", false)]
#[case::embedded_newline("hello\nworld", false)]
#[case::bracketed_paste_end_marker("he\x1b[201~llo", false)]
fn paste_safety_classifies_control_sequences(#[case] input: &str, #[case] safe: bool) {
    assert_eq!(paste::is_safe(input), safe);
}

fn sanitized_paste(input: &str) -> Vec<u8> {
    input
        .chars()
        .map(|character| match character {
            '\n' => '\r',
            character if character.is_control() && character != '\r' => ' ',
            character => character,
        })
        .collect::<String>()
        .into_bytes()
}

proptest! {
    /// Outside bracketed-paste mode, arbitrary text is normalized to printable bytes and carriage
    /// returns; line feeds, escapes, and deletes never reach the terminal encoder.
    #[test]
    fn unbracketed_paste_never_emits_terminal_control_bytes(input in "\\PC*") {
        let mut engine = test_terminal_engine().expect("terminal engine");
        let mut out = Vec::new();

        engine
            .encode_paste_to_vec(&input, &mut out)
            .expect("encode paste");

        prop_assert_eq!(&out, &sanitized_paste(&input));
    }

    /// In bracketed-paste mode, the encoder adds exactly one opening and closing marker around a
    /// payload with the same control-byte sanitization as ordinary paste.
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
        prop_assert_eq!(payload, sanitized_paste(&input));
    }
}
