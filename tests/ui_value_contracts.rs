use bootty_app::{
    config::{AppearanceVariant, BoottyConfig, ColorConfig},
    theme::{theme_from_config, theme_palette_from_colors},
    ui::{
        icons::install_icon_fonts,
        keycaps::trigger_galley,
        overlay::{
            ListOutcome, ListRow, ListView,
            list::{clamp_selection, selection_after_nav},
        },
        rename::{RenameSessionDialog, RenameSessionEvent, RenameTabDialog, RenameTabEvent},
    },
};
use egui::{Color32, Event, Key, Modifiers, RawInput};

fn key_event(key: Key) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }
}

fn show_list(
    context: &egui::Context,
    rows: &[ListRow],
    selected: usize,
    input: RawInput,
) -> ListOutcome {
    let mut outcome = None;
    let _ = context.run_ui(input, |ui| {
        outcome = Some(
            ListView::new("ui-value-contracts", rows, selected)
                .max_height(240.0)
                .show(ui, theme_palette_from_colors(&ColorConfig::default())),
        );
    });
    outcome.expect("the list renders")
}

#[test]
fn list_navigation_stays_within_the_available_rows() {
    assert_eq!(selection_after_nav(0, 3, true, false), 1);
    assert_eq!(selection_after_nav(2, 3, true, false), 2);
    assert_eq!(selection_after_nav(2, 3, false, true), 1);
    assert_eq!(selection_after_nav(0, 3, false, true), 0);
    assert_eq!(selection_after_nav(5, 0, true, true), 0);
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

    let next = show_list(
        &context,
        &rows,
        first.selected,
        RawInput {
            events: vec![key_event(Key::ArrowDown)],
            ..RawInput::default()
        },
    );
    assert_eq!(next.selected, 3);
}

#[test]
fn public_keycap_layout_normalizes_named_and_single_character_keys() {
    let context = egui::Context::default();
    let mut labels = Vec::new();
    let palette = theme_palette_from_colors(&ColorConfig::default());

    let _ = context.run_ui(RawInput::default(), |ui| {
        for trigger in ["p", "space", "escape", "esc", "enter"] {
            let galley = trigger_galley(ui, palette, trigger, Color32::WHITE, 320.0);
            labels.push(galley.job.text.clone());
        }
    });

    assert_eq!(labels, ["P", "Space", "Esc", "Esc", "Enter"]);
}

#[test]
fn session_rename_trims_the_submitted_name_and_rejects_blank_names() {
    let context = egui::Context::default();
    install_icon_fonts(&context);
    let mut dialog = RenameSessionDialog::open("session-1".to_owned(), "  review  ".to_owned());
    let mut event = RenameSessionEvent::None;
    let _ = context.run_ui(RawInput::default(), |ui| {
        event = dialog.show(
            ui.ctx(),
            theme_from_config(&BoottyConfig::default(), AppearanceVariant::Dark),
        );
    });
    let _ = context.run_ui(
        RawInput {
            events: vec![key_event(Key::Enter)],
            ..RawInput::default()
        },
        |ui| {
            event = dialog.show(
                ui.ctx(),
                theme_from_config(&BoottyConfig::default(), AppearanceVariant::Dark),
            );
        },
    );
    assert_eq!(
        event,
        RenameSessionEvent::Rename {
            session_id: "session-1".to_owned(),
            name: "review".to_owned(),
        }
    );

    let mut blank = RenameSessionDialog::open("session-2".to_owned(), "   ".to_owned());
    let _ = context.run_ui(RawInput::default(), |ui| {
        event = blank.show(
            ui.ctx(),
            theme_from_config(&BoottyConfig::default(), AppearanceVariant::Dark),
        );
    });
    let _ = context.run_ui(
        RawInput {
            events: vec![key_event(Key::Enter)],
            ..RawInput::default()
        },
        |ui| {
            event = blank.show(
                ui.ctx(),
                theme_from_config(&BoottyConfig::default(), AppearanceVariant::Dark),
            );
        },
    );
    assert_eq!(event, RenameSessionEvent::None);
}

#[test]
fn blank_tab_rename_restores_terminal_managed_titles() {
    let context = egui::Context::default();
    install_icon_fonts(&context);
    let mut dialog = RenameTabDialog::open(
        "session-1".to_owned(),
        "window-1".to_owned(),
        "   ".to_owned(),
    );
    let mut event = RenameTabEvent::None;
    let _ = context.run_ui(RawInput::default(), |ui| {
        event = dialog.show(
            ui.ctx(),
            theme_from_config(&BoottyConfig::default(), AppearanceVariant::Dark),
        );
    });
    let _ = context.run_ui(
        RawInput {
            events: vec![key_event(Key::Enter)],
            ..RawInput::default()
        },
        |ui| {
            event = dialog.show(
                ui.ctx(),
                theme_from_config(&BoottyConfig::default(), AppearanceVariant::Dark),
            );
        },
    );

    assert_eq!(
        event,
        RenameTabEvent::Rename {
            session_id: "session-1".to_owned(),
            window_id: "window-1".to_owned(),
            name: String::new(),
        }
    );
}
