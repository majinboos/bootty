use super::super::super::*;
use super::support::*;

fn small_terminal_engine(cols: u16, rows: u16) -> Result<TerminalEngine> {
    TerminalEngine::new_with_scrollback(
        TerminalGeometry {
            cols,
            rows,
            cell_width: 10,
            cell_height: 20,
        },
        TerminalColorConfig::default(),
        NATIVE_MAX_SCROLLBACK,
    )
}

fn copied_text(outcome: TerminalCopyModeOutcome) -> String {
    String::from_utf8(outcome.copied.expect("copy mode should copy text"))
        .expect("terminal copy mode output should be utf-8 text")
}

#[test]
fn copy_mode_select_line_copies_current_line_and_exits() -> Result<()> {
    let mut engine = small_terminal_engine(20, 4)?;
    engine.write_vt(b"first line\r\nsecond row");

    engine.enter_copy_mode()?;
    let frame = engine.extract_frame()?;
    assert_eq!(
        frame.copy_mode,
        Some(FrameCopyMode {
            selecting: false,
            rectangle: false,
        })
    );

    let outcome = engine.handle_copy_mode_action(TerminalCopyModeAction::SelectLine)?;
    assert!(outcome.active);
    let frame = engine.extract_frame()?;
    assert_eq!(
        frame.copy_mode,
        Some(FrameCopyMode {
            selecting: true,
            rectangle: false,
        })
    );

    let outcome = engine.handle_copy_mode_action(TerminalCopyModeAction::CopySelectionAndCancel)?;

    assert_eq!(copied_text(outcome), "second row");
    assert!(!engine.copy_mode_active());
    assert_eq!(engine.extract_frame()?.copy_mode, None);
    Ok(())
}

#[test]
fn copy_mode_visual_line_motion_keeps_whole_lines_selected() -> Result<()> {
    let mut engine = small_terminal_engine(20, 4)?;
    engine.write_vt(b"first line\r\nsecond row\r\nthird");

    engine.enter_copy_mode()?;
    engine.handle_copy_mode_action(TerminalCopyModeAction::SelectLine)?;
    engine.handle_copy_mode_action(TerminalCopyModeAction::Move(TerminalCopyModeMotion::Up))?;
    let outcome = engine.handle_copy_mode_action(TerminalCopyModeAction::CopySelectionAndCancel)?;

    assert_eq!(copied_text(outcome), "second row\nthird");
    Ok(())
}

#[test]
fn copy_mode_toggle_selection_end_moves_cursor_between_ends() -> Result<()> {
    let mut engine = small_terminal_engine(20, 4)?;
    engine.write_vt(b"alpha beta");

    engine.enter_copy_mode()?;
    engine.handle_copy_mode_action(TerminalCopyModeAction::Move(
        TerminalCopyModeMotion::StartOfLine,
    ))?;
    engine.handle_copy_mode_action(TerminalCopyModeAction::BeginSelection)?;
    engine.handle_copy_mode_action(TerminalCopyModeAction::Move(
        TerminalCopyModeMotion::NextWordEnd,
    ))?;
    assert_eq!(engine.extract_frame()?.cursor.expect("cursor").x, 4);

    engine.handle_copy_mode_action(TerminalCopyModeAction::ToggleSelectionEnd)?;
    assert_eq!(engine.extract_frame()?.cursor.expect("cursor").x, 0);
    Ok(())
}

#[test]
fn copy_mode_visual_selection_uses_vim_word_motion() -> Result<()> {
    let mut engine = small_terminal_engine(20, 4)?;
    engine.write_vt(b"alpha beta");

    engine.enter_copy_mode()?;
    engine.handle_copy_mode_action(TerminalCopyModeAction::Move(
        TerminalCopyModeMotion::StartOfLine,
    ))?;
    engine.handle_copy_mode_action(TerminalCopyModeAction::BeginSelection)?;
    engine.handle_copy_mode_action(TerminalCopyModeAction::Move(
        TerminalCopyModeMotion::NextWordEnd,
    ))?;

    let outcome = engine.handle_copy_mode_action(TerminalCopyModeAction::CopySelectionAndCancel)?;

    assert_eq!(copied_text(outcome), "alpha");
    assert!(!engine.copy_mode_active());
    Ok(())
}

#[test]
fn copy_mode_next_word_from_space_stops_at_immediate_word() -> Result<()> {
    let mut engine = small_terminal_engine(20, 4)?;
    engine.write_vt(b"alpha beta gamma");

    engine.enter_copy_mode()?;
    engine.handle_copy_mode_action(TerminalCopyModeAction::Move(
        TerminalCopyModeMotion::StartOfLine,
    ))?;
    for _ in 0..5 {
        engine
            .handle_copy_mode_action(TerminalCopyModeAction::Move(TerminalCopyModeMotion::Right))?;
    }
    engine.handle_copy_mode_action(TerminalCopyModeAction::Move(
        TerminalCopyModeMotion::NextWord,
    ))?;

    let cursor = engine.extract_frame()?.cursor.expect("copy-mode cursor");
    assert_eq!(cursor.x, 6);
    Ok(())
}

#[test]
fn copy_mode_visual_toggle_disables_selection() -> Result<()> {
    let mut engine = small_terminal_engine(20, 4)?;
    engine.write_vt(b"alpha beta");

    engine.enter_copy_mode()?;
    engine.handle_copy_mode_action(TerminalCopyModeAction::ToggleSelection)?;
    assert!(
        engine
            .extract_frame()?
            .copy_mode
            .is_some_and(|mode| mode.selecting && !mode.rectangle)
    );

    engine.handle_copy_mode_action(TerminalCopyModeAction::ToggleSelection)?;
    assert!(
        engine
            .extract_frame()?
            .copy_mode
            .is_some_and(|mode| !mode.selecting && !mode.rectangle)
    );
    Ok(())
}

#[test]
fn copy_mode_page_motion_keeps_cursor_visible_in_scrollback() -> Result<()> {
    let mut engine = small_terminal_engine(12, 2)?;
    engine.write_vt(b"one\r\ntwo\r\nthree\r\nfour");

    engine.enter_copy_mode()?;
    engine.handle_copy_mode_action(TerminalCopyModeAction::Move(TerminalCopyModeMotion::PageUp))?;
    let frame = engine.extract_frame()?;

    assert!(frame.copy_mode.is_some());
    assert!(frame.cursor.is_some(), "copy-mode cursor should be visible");
    assert!(
        frame.scrollbar.expect("scrollbar").offset > 0,
        "page-up should scroll the viewport into history"
    );
    Ok(())
}

#[test]
fn copy_mode_line_motion_scrolls_past_viewport_top() -> Result<()> {
    let mut engine = small_terminal_engine(12, 2)?;
    engine.write_vt(b"one\r\ntwo\r\nthree\r\nfour");

    engine.enter_copy_mode()?;
    engine.handle_copy_mode_action(TerminalCopyModeAction::Move(TerminalCopyModeMotion::Up))?;
    engine.handle_copy_mode_action(TerminalCopyModeAction::Move(TerminalCopyModeMotion::Up))?;
    let frame = engine.extract_frame()?;
    assert_eq!(row_text(frame, 0), "two", "scrollbar={:?}", frame.scrollbar);

    assert!(frame.copy_mode.is_some());
    assert!(
        frame.cursor.is_some(),
        "copy-mode cursor should remain visible"
    );
    let scrollbar = frame.scrollbar.expect("scrollbar");
    assert!(
        scrollbar.offset < scrollbar.total.saturating_sub(scrollbar.len),
        "repeated up motions should scroll into history instead of stopping at the viewport bottom"
    );
    assert_eq!(frame.cursor.expect("copy-mode cursor").y, 0);
    Ok(())
}

#[test]
fn copy_mode_line_motion_reaches_scrollback_top() -> Result<()> {
    let mut engine = small_terminal_engine(12, 2)?;
    engine.write_vt(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix");

    engine.enter_copy_mode()?;
    for _ in 0..12 {
        engine.handle_copy_mode_action(TerminalCopyModeAction::Move(TerminalCopyModeMotion::Up))?;
    }
    let frame = engine.extract_frame()?;
    assert_eq!(row_text(frame, 0), "one", "scrollbar={:?}", frame.scrollbar);
    let scrollbar = frame.scrollbar.expect("scrollbar");

    assert_eq!(scrollbar.offset, 0);
    assert_eq!(frame.cursor.expect("copy-mode cursor").y, 0);
    Ok(())
}

#[test]
fn copy_mode_scroll_motion_keeps_cursor_attached_to_viewport() -> Result<()> {
    let mut engine = small_terminal_engine(12, 2)?;
    engine.write_vt(b"one\r\ntwo\r\nthree\r\nfour");

    engine.enter_copy_mode()?;
    engine.handle_copy_mode_action(TerminalCopyModeAction::Move(
        TerminalCopyModeMotion::ScrollUp,
    ))?;
    let frame = engine.extract_frame()?;
    assert_eq!(row_text(frame, 0), "two");

    assert!(frame.copy_mode.is_some());
    assert_eq!(frame.cursor.expect("copy-mode cursor").y, 1);
    assert!(
        frame.scrollbar.expect("scrollbar").offset > 0,
        "copy-mode scroll-up should move the viewport instead of being undone by cursor visibility"
    );
    Ok(())
}

#[test]
fn copy_mode_search_query_moves_cursor_and_scrolls_to_history_match() -> Result<()> {
    let mut engine = small_terminal_engine(16, 2)?;
    engine.write_vt(b"one target\r\ntwo\r\nthree\r\nfour");

    engine.enter_copy_mode()?;
    let outcome = engine.handle_copy_mode_action(TerminalCopyModeAction::Search {
        query: "target".to_owned(),
        direction: TerminalSearchDirection::Previous,
    })?;
    let frame = engine.extract_frame()?;

    assert_eq!(
        outcome.search,
        Some(TerminalCopyModeSearchOutcome {
            query: "target".to_owned(),
            found: true,
        })
    );
    assert!(outcome.active);
    assert_eq!(
        row_text(frame, 0),
        "one target",
        "scrollbar={:?}",
        frame.scrollbar
    );
    assert_eq!(frame.scrollbar.expect("scrollbar").offset, 0);
    assert_eq!(frame.cursor.expect("copy-mode cursor").x, 4);
    Ok(())
}

#[test]
fn copy_mode_search_matches_across_soft_wrapped_rows() -> Result<()> {
    let mut engine = small_terminal_engine(5, 3)?;
    engine.write_vt(b"abcdeFGH");

    engine.enter_copy_mode()?;
    let outcome = engine.handle_copy_mode_action(TerminalCopyModeAction::Search {
        query: "eF".to_owned(),
        direction: TerminalSearchDirection::Previous,
    })?;
    let frame = engine.extract_frame()?;

    assert_eq!(
        outcome.search,
        Some(TerminalCopyModeSearchOutcome {
            query: "eF".to_owned(),
            found: true,
        })
    );
    assert_eq!(
        row_text(frame, 0),
        "abcde",
        "row_wraps={:?}",
        frame.row_wraps
    );
    assert!(frame.row_wraps.first().copied().unwrap_or(false));
    let cursor = frame.cursor.expect("copy-mode cursor");
    assert_eq!((cursor.x, cursor.y), (4, 0));
    Ok(())
}

#[test]
fn copy_mode_search_query_moves_forward_and_backward_between_matches() -> Result<()> {
    let mut engine = small_terminal_engine(20, 3)?;
    engine.write_vt(b"foo bar\r\nbaz foo\r\nqux");

    engine.enter_copy_mode()?;
    engine.handle_copy_mode_action(TerminalCopyModeAction::Move(
        TerminalCopyModeMotion::HistoryTop,
    ))?;

    let outcome = engine.handle_copy_mode_action(TerminalCopyModeAction::Search {
        query: "foo".to_owned(),
        direction: TerminalSearchDirection::Next,
    })?;
    let frame = engine.extract_frame()?;
    assert_eq!(
        outcome.search,
        Some(TerminalCopyModeSearchOutcome {
            query: "foo".to_owned(),
            found: true,
        })
    );
    let cursor = frame.cursor.expect("copy-mode cursor");
    assert_eq!((cursor.x, cursor.y), (4, 1));

    let outcome = engine.handle_copy_mode_action(TerminalCopyModeAction::Search {
        query: "foo".to_owned(),
        direction: TerminalSearchDirection::Previous,
    })?;
    let frame = engine.extract_frame()?;
    assert_eq!(
        outcome.search,
        Some(TerminalCopyModeSearchOutcome {
            query: "foo".to_owned(),
            found: true,
        })
    );
    let cursor = frame.cursor.expect("copy-mode cursor");
    assert_eq!((cursor.x, cursor.y), (0, 0));
    Ok(())
}

#[test]
fn copy_mode_search_word_uses_word_under_cursor() -> Result<()> {
    let mut engine = small_terminal_engine(24, 3)?;
    engine.write_vt(b"alpha beta alpha");

    engine.enter_copy_mode()?;
    engine.handle_copy_mode_action(TerminalCopyModeAction::Move(
        TerminalCopyModeMotion::StartOfLine,
    ))?;

    let outcome = engine.handle_copy_mode_action(TerminalCopyModeAction::SearchWord(
        TerminalSearchDirection::Next,
    ))?;
    let frame = engine.extract_frame()?;
    assert_eq!(
        outcome.search,
        Some(TerminalCopyModeSearchOutcome {
            query: "alpha".to_owned(),
            found: true,
        })
    );
    assert_eq!(frame.cursor.expect("copy-mode cursor").x, 11);

    let outcome = engine.handle_copy_mode_action(TerminalCopyModeAction::SearchWord(
        TerminalSearchDirection::Previous,
    ))?;
    let frame = engine.extract_frame()?;
    assert_eq!(
        outcome.search,
        Some(TerminalCopyModeSearchOutcome {
            query: "alpha".to_owned(),
            found: true,
        })
    );
    assert_eq!(frame.cursor.expect("copy-mode cursor").x, 0);
    Ok(())
}

#[test]
fn copy_mode_escape_first_clears_selection_then_exits() -> Result<()> {
    let mut engine = test_terminal_engine()?;
    engine.write_vt(b"copy me");

    engine.enter_copy_mode()?;
    engine.handle_copy_mode_action(TerminalCopyModeAction::SelectLine)?;
    assert!(
        engine
            .extract_frame()?
            .copy_mode
            .is_some_and(|mode| mode.selecting)
    );

    let outcome = engine.handle_copy_mode_action(TerminalCopyModeAction::CancelOrClearSelection)?;
    assert!(outcome.active);
    assert!(
        engine
            .extract_frame()?
            .copy_mode
            .is_some_and(|mode| !mode.selecting)
    );

    let outcome = engine.handle_copy_mode_action(TerminalCopyModeAction::CancelOrClearSelection)?;
    assert!(!outcome.active);
    assert!(!engine.copy_mode_active());
    Ok(())
}
