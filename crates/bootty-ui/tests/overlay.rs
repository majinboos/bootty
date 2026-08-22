use bootty_ui::{
    ThemePalette,
    overlay::{ListOutcome, ListRow, ListView, clamp_selection, fuzzy_match},
};
use eframe::egui::{self, Event, Key, Modifiers, RawInput};
use pretty_assertions::assert_eq;

fn key_event(key: Key, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

fn key_input(key: Key) -> RawInput {
    modified_key_input(key, Modifiers::NONE)
}

fn modified_key_input(key: Key, modifiers: Modifiers) -> RawInput {
    RawInput {
        events: vec![
            Event::ModifiersChanged(modifiers),
            key_event(key, modifiers),
        ],
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
    show_list_hover(context, rows, selected, input, true)
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
fn list_supports_control_navigation_and_enter_activation() {
    let context = egui::Context::default();
    let rows = rows(&["one", "two"]);
    let control = Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    };

    let next = show_list(&context, &rows, 0, modified_key_input(Key::N, control));
    assert_eq!(next.selected, 1);

    let activated = show_list(&context, &rows, next.selected, key_input(Key::Enter));
    assert_eq!(activated.activated, Some(1));

    let previous = show_list(
        &context,
        &rows,
        activated.selected,
        modified_key_input(Key::P, control),
    );
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

    let normalized = show_list(&context, &rows, 0, key_input(Key::ArrowDown));
    assert_eq!(normalized.selected, 1);

    let next = show_list(&context, &rows, first.selected, key_input(Key::ArrowDown));
    assert_eq!(next.selected, 3);
}

#[test]
fn overlay_search_matches_case_insensitive_subsequences() {
    for (candidate, query, expected) in [
        ("bootty", "bty", true),
        ("Dotfiles", "df", true),
        ("bootty", "xyz", false),
        ("ab", "abc", false),
    ] {
        assert_eq!(fuzzy_match(candidate, query), expected);
    }
}

/// A confirmation list pre-selects the safe action on purpose; the pointer passing over a
/// destructive row must not make it what Enter does.
#[test]
fn a_list_that_does_not_hover_select_keeps_its_selection_under_the_pointer() {
    let context = egui::Context::default();
    let rows = rows(&["Detach worktree", "Delete branch"]);
    // Settle the list once so the pointer counts as having moved between frames.
    let pointer_over_second_row = |x: f32, y: f32| RawInput {
        events: vec![Event::PointerMoved(egui::pos2(x, y))],
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(400.0, 200.0),
        )),
        ..RawInput::default()
    };
    let mut selected = 0;
    for frame in 0..2 {
        let outcome = show_list_hover(
            &context,
            &rows,
            selected,
            pointer_over_second_row(20.0, 40.0 + frame as f32),
            false,
        );
        selected = outcome.selected;
    }
    assert_eq!(selected, 0, "the pointer must not move the selection");

    let context = egui::Context::default();
    let mut selected = 0;
    for frame in 0..2 {
        let outcome = show_list_hover(
            &context,
            &rows,
            selected,
            pointer_over_second_row(20.0, 40.0 + frame as f32),
            true,
        );
        selected = outcome.selected;
    }
    assert_eq!(
        selected, 1,
        "a hover-selecting list still follows the pointer"
    );
}

fn show_list_hover(
    context: &egui::Context,
    rows: &[ListRow],
    selected: usize,
    input: RawInput,
    hover_selects: bool,
) -> ListOutcome {
    let mut outcome = None;
    context
        .run_ui(input, |ui| {
            outcome = Some(
                ListView::new("hover-selects", rows, selected)
                    .max_height(240.0)
                    .hover_selects(hover_selects)
                    .show(ui, ThemePalette::default()),
            );
        })
        .drop_without_applying_deltas();
    outcome.expect("the list renders")
}
