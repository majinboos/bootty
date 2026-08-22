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

#[test]
fn synchronized_output_observation_covers_a_completed_batch() {
    let mut engine = test_terminal_engine().expect("terminal engine");

    engine.write_vt(b"\x1b[?2026hredraw\x1b[?2026l");

    assert!(engine.take_synchronized_output_observed());
    assert!(!engine.take_synchronized_output_observed());
    assert!(
        !engine
            .is_synchronized_output()
            .expect("query sync output mode")
    );
}

#[test]
fn synchronized_output_observation_covers_a_split_start() {
    let mut engine = test_terminal_engine().expect("terminal engine");

    engine.write_vt(b"\x1b[?20");
    assert!(!engine.take_synchronized_output_observed());
    engine.write_vt(b"26hredraw");

    assert!(engine.take_synchronized_output_observed());
    assert!(
        engine
            .is_synchronized_output()
            .expect("query sync output mode")
    );
}
