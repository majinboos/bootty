use bootty_ui::{
    ThemePalette,
    settings::{
        NumberEditSpec, apply_reorder, settings_number_edit, settings_segmented, settings_toggle,
    },
};
use eframe::egui::{self, Event, Modifiers, PointerButton, Pos2, RawInput, Rect};
use pretty_assertions::assert_eq;

fn run_frame(context: &egui::Context, events: Vec<Event>, mut show: impl FnMut(&mut egui::Ui)) {
    context
        .run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(400.0, 240.0))),
                events,
                ..RawInput::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, &mut show);
            },
        )
        .drop_without_applying_deltas();
}

fn pointer_button(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

#[test]
fn reorder_slot_moves_items_and_preserves_invalid_sources() {
    let mut items = vec!["one", "two", "three"];
    apply_reorder(&mut items, 0, 3);
    assert_eq!(items, ["two", "three", "one"]);

    apply_reorder(&mut items, 2, 0);
    assert_eq!(items, ["one", "two", "three"]);

    apply_reorder(&mut items, 1, 1);
    apply_reorder(&mut items, 1, 2);
    assert_eq!(items, ["one", "two", "three"]);

    apply_reorder(&mut items, 99, 0);
    assert_eq!(items, ["one", "two", "three"]);
}

#[test]
fn segmented_selection_reports_the_clicked_segment() {
    let context = egui::Context::default();
    let mut changed = None;
    let position = Pos2::new(130.0, 25.0);

    run_frame(&context, vec![Event::PointerMoved(position)], |ui| {
        changed = settings_segmented(ui, ThemePalette::default(), &["Left", "Right"], 0);
    });
    run_frame(&context, vec![pointer_button(position, true)], |ui| {
        changed = settings_segmented(ui, ThemePalette::default(), &["Left", "Right"], 0);
    });
    run_frame(&context, vec![pointer_button(position, false)], |ui| {
        changed = settings_segmented(ui, ThemePalette::default(), &["Left", "Right"], 0);
    });

    assert_eq!(changed, Some(1));
}

#[test]
fn number_edit_formats_value_with_precision_and_suffix() {
    let context = egui::Context::default();
    let spec = || NumberEditSpec {
        id_salt: &["settings", "number"],
        range: 0.0..=1.0,
        suffix: " %",
        precision: 2,
        display_scale: 100.0,
    };
    let mut value = 0.5;
    let mut changed = true;
    let mut edit_id = None;

    run_frame(&context, Vec::new(), |ui| {
        edit_id = Some(ui.make_persistent_id(("settings-number-edit", "settings.number")));
        changed = settings_number_edit(ui, ThemePalette::default(), &mut value, spec());
    });

    assert!(!changed);
    let formatted = context.memory(|memory| {
        memory
            .data
            .get_temp::<String>(edit_id.expect("number edit id"))
    });
    assert_eq!(formatted.as_deref(), Some("50.00 %"));
}

#[test]
fn toggle_reports_a_click_and_flips_the_value() {
    let context = egui::Context::default();
    let mut value = false;

    run_frame(
        &context,
        vec![Event::PointerMoved(Pos2::new(23.0, 13.0))],
        |ui| {
            let _ = settings_toggle(ui, ThemePalette::default(), &mut value);
        },
    );
    run_frame(
        &context,
        vec![pointer_button(Pos2::new(23.0, 13.0), true)],
        |ui| {
            let _ = settings_toggle(ui, ThemePalette::default(), &mut value);
        },
    );
    let mut changed = false;
    run_frame(
        &context,
        vec![pointer_button(Pos2::new(23.0, 13.0), false)],
        |ui| {
            changed = settings_toggle(ui, ThemePalette::default(), &mut value);
        },
    );

    assert!(changed);
    assert!(value);
}
