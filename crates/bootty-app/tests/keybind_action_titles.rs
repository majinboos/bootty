use bootty_app::ui::settings::surface::keybinds::{action_title, humanize_action};

#[test]
fn action_titles_prefer_catalog_titles_and_keep_params() {
    assert_eq!(action_title("reload_config"), "Reload Config");
    assert_eq!(action_title("paste_from_clipboard"), "Paste");
    assert_eq!(
        action_title("decrease_font_size:1"),
        "Decrease Font Size: 1"
    );
    assert_eq!(
        action_title("change_appearance:dark"),
        "Use Dark Appearance"
    );
}

#[test]
fn humanize_action_sentence_cases_names_off_the_catalog() {
    assert_eq!(humanize_action("focus_terminal"), "Focus terminal");
}
