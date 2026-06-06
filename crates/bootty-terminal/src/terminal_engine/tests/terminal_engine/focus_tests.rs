use super::support::*;
use rstest::rstest;

#[test]
fn terminal_engine_encodes_focus_reports_only_when_enabled() {
    let mut engine = test_terminal_engine().expect("terminal engine");
    let mut out = Vec::new();

    engine
        .encode_focus_to_vec(true, &mut out)
        .expect("focus without reporting mode");
    assert!(out.is_empty());

    engine.write_vt(b"\x1b[?1004h");
    for (gained, expected) in [(true, b"\x1b[I".as_slice()), (false, b"\x1b[O".as_slice())] {
        engine
            .encode_focus_to_vec(gained, &mut out)
            .expect("focus report");
        assert_eq!(out, expected);
    }
}

#[rstest]
#[case::enabled_by_decset_1004(true, b"\x1b[I".as_slice())]
#[case::lost_focus(false, b"\x1b[O".as_slice())]
fn focus_reports_match_csi_focus_protocol(#[case] gained: bool, #[case] expected: &[u8]) {
    let mut engine = test_terminal_engine().expect("terminal engine");
    let mut out = Vec::new();
    engine.write_vt(b"\x1b[?1004h");

    engine
        .encode_focus_to_vec(gained, &mut out)
        .expect("focus report");

    assert_eq!(out, expected);
}
