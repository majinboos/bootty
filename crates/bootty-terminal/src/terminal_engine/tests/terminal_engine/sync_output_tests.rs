use super::support::*;

#[test]
fn synchronized_output_mode_follows_vt_2026() {
    let mut engine = test_terminal_engine().expect("terminal engine");
    assert!(
        !engine
            .is_synchronized_output()
            .expect("query sync output mode")
    );

    engine.write_vt(b"\x1b[?2026h");
    assert!(
        engine
            .is_synchronized_output()
            .expect("query sync output mode")
    );

    engine.write_vt(b"\x1b[?2026l");
    assert!(
        !engine
            .is_synchronized_output()
            .expect("query sync output mode")
    );
}
