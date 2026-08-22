use bootty_ui::code_editor::{
    dialect_syntax, is_comment_shortcut, line_numbers, toggle_comments_in,
};
use eframe::egui;

#[test]
fn gutter_numbers_only_existing_source_lines() {
    assert_eq!(line_numbers("first\nsecond"), "1\n2");
    assert_eq!(line_numbers("first\n"), "1\n2");
}

#[test]
fn dialect_keywords_join_the_base_syntax() {
    let syntax = dialect_syntax(&["continue", "export", "type"]);
    assert!(syntax.is_keyword("continue"));
    assert!(syntax.is_keyword("export"));
    assert!(syntax.is_keyword("type"));
    assert!(syntax.is_keyword("local"));
}

#[test]
fn comment_shortcut_accepts_logical_questionmark_and_physical_slash() {
    let event = egui::Event::Key {
        key: egui::Key::Questionmark,
        physical_key: Some(egui::Key::Slash),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND,
    };

    assert!(is_comment_shortcut(&event));
}

#[test]
fn comment_toggle_handles_the_current_line() {
    let mut source = "local value = 1".to_owned();
    let cursor = egui::text::CCursorRange::one(egui::text::CCursor::new(6));

    let cursor = toggle_comments_in(&mut source, cursor);
    assert_eq!(source, "-- local value = 1");
    assert_eq!(usize::from(cursor.primary.index), 9);

    let cursor = toggle_comments_in(&mut source, cursor);
    assert_eq!(source, "local value = 1");
    assert_eq!(usize::from(cursor.primary.index), 6);
}

#[test]
fn comment_toggle_handles_selected_lines() {
    let original = "  local a\n  local b\nnext";
    let mut source = original.to_owned();
    let selection =
        egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(19));

    let selection = toggle_comments_in(&mut source, selection);
    assert_eq!(source, "  -- local a\n  -- local b\nnext");

    toggle_comments_in(&mut source, selection);
    assert_eq!(source, original);
}
