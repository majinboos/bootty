use bootty_app::{
    config::{AppearanceVariant, BoottyConfig},
    theme::theme_from_config,
    ui::{
        icons::install_icon_fonts,
        new_session_picker::{NewMuxSessionDialog, NewSessionPickerEvent},
    },
};
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

fn show(dialog: &mut NewMuxSessionDialog, input: RawInput) -> NewSessionPickerEvent {
    let context = egui::Context::default();
    install_icon_fonts(&context);
    let mut event = NewSessionPickerEvent::None;
    context
        .run_ui(input, |ui| {
            event = dialog.show(
                ui.ctx(),
                theme_from_config(&BoottyConfig::default(), AppearanceVariant::Dark),
                &[],
            );
        })
        .drop_without_applying_deltas();
    event
}

#[test]
fn new_session_picker_stays_open_without_a_dismiss_action() {
    let mut dialog = NewMuxSessionDialog::open();

    assert_eq!(
        show(&mut dialog, RawInput::default()),
        NewSessionPickerEvent::None
    );
}

#[test]
fn escape_closes_the_new_session_picker() {
    let mut dialog = NewMuxSessionDialog::open();

    assert_eq!(
        show(
            &mut dialog,
            RawInput {
                events: vec![key_event(Key::Escape)],
                ..RawInput::default()
            },
        ),
        NewSessionPickerEvent::Close
    );
}
