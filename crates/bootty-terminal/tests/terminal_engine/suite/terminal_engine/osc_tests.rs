use super::super::super::*;
use pretty_assertions::assert_eq;
#[test]
fn terminal_engine_tracks_completed_working_directory_reports() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 80,
        rows: 5,
        cell_width: 8,
        cell_height: 16,
    })?;

    engine.write_vt(b"\x1b]7;file:///tmp/example\x07");
    assert_eq!(engine.current_working_directory(), "file:///tmp/example");

    engine.write_vt(b"\x1b]7;file:///tmp/split");
    assert_eq!(engine.current_working_directory(), "file:///tmp/example");
    engine.write_vt(b"-path\x1b\\");
    assert_eq!(engine.current_working_directory(), "file:///tmp/split-path");

    Ok(())
}

#[test]
fn terminal_engine_reports_bootty_ports_from_iterm2_user_var() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 80,
        rows: 5,
        cell_width: 8,
        cell_height: 16,
    })?;

    engine.write_vt(b"\x1b]1337;SetUserVar=bootty_ports=ODA4MCwzMDAw\x07");

    assert_eq!(
        engine.drain_side_effects(),
        vec![TerminalSideEffect::Iterm2UserVarPorts(vec![8080, 3000])]
    );
    Ok(())
}

#[test]
fn terminal_engine_tracks_tmux_wrapped_working_directory_reports() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 80,
        rows: 5,
        cell_width: 8,
        cell_height: 16,
    })?;

    engine.write_vt(b"\x1bPtmux;\x1b\x1b]7;file:///tmp/wrapped\x1b\x1b\\\x1b\\");

    assert_eq!(engine.current_working_directory(), "file:///tmp/wrapped");

    Ok(())
}
