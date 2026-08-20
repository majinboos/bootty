use bootty_app::{
    config::{AppearanceVariant, BoottyConfig},
    direct_input::ModifierSideState,
    theme::theme_from_config,
    ui::settings::{SettingsAction, SettingsSurface},
};
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
fn settings_close_when_escape_has_no_active_editor() {
    let config = BoottyConfig::default();
    let theme = theme_from_config(&config, AppearanceVariant::Dark);
    let mut settings = SettingsSurface::new(config);
    let context = egui::Context::default();
    let mut action = SettingsAction::None;

    context
        .run_ui(
            RawInput {
                events: vec![key_event(Key::Escape)],
                ..RawInput::default()
            },
            |ui| {
                action = settings.show(ui, theme, Vec::new(), ModifierSideState::default());
            },
        )
        .drop_without_applying_deltas();

    assert_eq!(action, SettingsAction::Close);
}

#[test]
fn settings_restore_the_callers_global_style_after_close() {
    let config = BoottyConfig::default();
    let theme = theme_from_config(&config, AppearanceVariant::Dark);
    let mut settings = SettingsSurface::new(config);
    let context = egui::Context::default();
    install_icon_fonts(&context);
    let original_interact_height = 51.0;
    context.global_style_mut(|style| style.spacing.interact_size.y = original_interact_height);

    context
        .run_ui(RawInput::default(), |ui| {
            let _ = settings.show(ui, theme, Vec::new(), ModifierSideState::default());
        })
        .drop_without_applying_deltas();
    assert_eq!(context.global_style().spacing.interact_size.y, 34.0);

    settings.restore_global_style(&context);

    assert_eq!(
        context.global_style().spacing.interact_size.y,
        original_interact_height
    );
}
