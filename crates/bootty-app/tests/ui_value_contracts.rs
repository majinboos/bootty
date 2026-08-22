use bootty_app::{
    theme::theme_from_config,
    ui::rename::{RenameSessionDialog, RenameSessionEvent, RenameTabDialog, RenameTabEvent},
};
use bootty_config::config::{AppearanceVariant, BoottyConfig};
use bootty_ui::icons::install_icon_fonts;
use egui::{Event, Key, Modifiers, RawInput};

fn key_event(key: Key) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }
}

#[test]
fn session_rename_trims_the_submitted_name_and_rejects_blank_names() {
    let context = egui::Context::default();
    install_icon_fonts(&context);
    let mut dialog = RenameSessionDialog::open("session-1".to_owned(), "  review  ".to_owned());
    let mut event = None;
    context
        .run_ui(RawInput::default(), |ui| {
            event = dialog.show(
                ui.ctx(),
                theme_from_config(&BoottyConfig::default(), AppearanceVariant::Dark),
            );
        })
        .drop_without_applying_deltas();
    context
        .run_ui(
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
        )
        .drop_without_applying_deltas();
    assert_eq!(
        event,
        Some(RenameSessionEvent::Rename {
            session_id: "session-1".to_owned(),
            name: "review".to_owned(),
        })
    );

    let mut blank = RenameSessionDialog::open("session-2".to_owned(), "   ".to_owned());
    context
        .run_ui(RawInput::default(), |ui| {
            event = blank.show(
                ui.ctx(),
                theme_from_config(&BoottyConfig::default(), AppearanceVariant::Dark),
            );
        })
        .drop_without_applying_deltas();
    context
        .run_ui(
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
        )
        .drop_without_applying_deltas();
    assert_eq!(event, None);
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
    let mut event = None;
    context
        .run_ui(RawInput::default(), |ui| {
            event = dialog.show(
                ui.ctx(),
                theme_from_config(&BoottyConfig::default(), AppearanceVariant::Dark),
            );
        })
        .drop_without_applying_deltas();
    context
        .run_ui(
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
        )
        .drop_without_applying_deltas();

    assert_eq!(
        event,
        Some(RenameTabEvent::Rename {
            session_id: "session-1".to_owned(),
            window_id: "window-1".to_owned(),
            name: String::new(),
        })
    );
}
