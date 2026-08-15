use bootty_ui::{
    ThemePalette,
    overlay::{ListOutcome, ListRow, ListView, clamp_selection, fuzzy_match},
};
use eframe::egui::{self, Event, Key, Modifiers, RawInput};

fn key_event(key: Key) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }
}

fn key_input(key: Key) -> RawInput {
    RawInput {
        events: vec![key_event(key)],
        ..RawInput::default()
    }
}

fn rows(labels: &[&str]) -> Vec<ListRow> {
    labels
        .iter()
        .map(|primary| ListRow {
            primary: (*primary).to_owned(),
            ..ListRow::default()
        })
        .collect()
}

fn show_list(
    context: &egui::Context,
    rows: &[ListRow],
    selected: usize,
    input: RawInput,
) -> ListOutcome {
    let mut outcome = None;
    context
        .run_ui(input, |ui| {
            outcome = Some(
                ListView::new("ui-value-contracts", rows, selected)
                    .max_height(240.0)
                    .show(ui, ThemePalette::default()),
            );
        })
        .drop_without_applying_deltas();
    outcome.expect("the list renders")
}

#[test]
fn list_navigation_stays_within_the_available_rows() {
    let context = egui::Context::default();
    let rows = rows(&["one", "two", "three"]);

    let first = show_list(&context, &rows, 0, RawInput::default());
    assert_eq!(first.selected, 0);

    let next = show_list(&context, &rows, first.selected, key_input(Key::ArrowDown));
    assert_eq!(next.selected, 1);

    let last = show_list(&context, &rows, 2, key_input(Key::ArrowDown));
    assert_eq!(last.selected, 2);

    let previous = show_list(&context, &rows, 0, key_input(Key::ArrowUp));
    assert_eq!(previous.selected, 0);
}

#[test]
fn stored_list_selection_is_clamped_after_rows_disappear() {
    assert_eq!(clamp_selection(0, 0), 0);
    assert_eq!(clamp_selection(9, 3), 2);
    assert_eq!(clamp_selection(1, 3), 1);
}

#[test]
fn list_navigation_skips_section_rows() {
    let context = egui::Context::default();
    let rows = vec![
        ListRow {
            primary: "Local".to_owned(),
            section: true,
            ..ListRow::default()
        },
        ListRow {
            primary: "local session".to_owned(),
            ..ListRow::default()
        },
        ListRow {
            primary: "Remote".to_owned(),
            section: true,
            ..ListRow::default()
        },
        ListRow {
            primary: "remote session".to_owned(),
            ..ListRow::default()
        },
    ];

    let first = show_list(&context, &rows, 0, RawInput::default());
    assert_eq!(first.selected, 1);

    let next = show_list(&context, &rows, first.selected, key_input(Key::ArrowDown));
    assert_eq!(next.selected, 3);
}

#[test]
fn overlay_search_uses_a_case_insensitive_subsequence() {
    assert!(fuzzy_match("bootty", "bty"));
    assert!(fuzzy_match("Dotfiles", "df"));
    assert!(fuzzy_match("anything", ""));
    assert!(!fuzzy_match("bootty", "xyz"));
    assert!(!fuzzy_match("ab", "abc"));
}
