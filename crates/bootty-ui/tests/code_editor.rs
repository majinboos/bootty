use bootty_ui::code_editor::{
    dialect_syntax, is_comment_shortcut, line_numbers, toggle_comments_in,
};
use eframe::egui;
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

#[rstest]
#[case("", "1")]
#[case("first\nsecond", "1\n2")]
#[case("first\n", "1\n2")]
fn gutter_numbers_each_logical_source_line(#[case] source: &str, #[case] expected: &str) {
    assert_eq!(line_numbers(source), expected);
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

proptest! {
    /// Property: commenting and uncommenting a whole plain document is lossless.
    #[test]
    fn toggling_plain_documents_twice_restores_source(
        lines in prop::collection::vec("[a-zA-Z0-9 ]{0,32}", 1..16),
    ) {
        let original = lines.join("\n");
        let mut source = original.clone();
        let selection = egui::text::CCursorRange::two(
            egui::text::CCursor::new(0),
            egui::text::CCursor::new(source.len()),
        );

        let commented = toggle_comments_in(&mut source, selection);
        toggle_comments_in(&mut source, commented);

        prop_assert_eq!(source, original);
    }
}
