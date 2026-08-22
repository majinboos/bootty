use std::collections::HashMap;

use bootty_app::{
    AppState, ModalDialog,
    theme::theme_from_config,
    ui::{
        ditch::{DitchAction, DitchSessionDialog, DitchSessionEvent},
        keybind_help::KeybindHelpDialog,
        session_navigation::BindingSessionGroup,
        session_picker::{SessionPickerDialog, SessionPickerEvent},
        space::{SpaceEditorDialog, SpaceEditorIntent},
        theme_picker::{ThemePickerDialog, ThemePickerEvent},
    },
};
use bootty_config::config::{AppearanceVariant, BoottyConfig};
use bootty_mux::{
    controller::{BindingId, MuxScope, SpaceId},
    snapshot::{MuxPaneAnchor, MuxSession, MuxSessionTag},
};
use bootty_ui::icons::install_icon_fonts;
use bootty_workspace::SpaceMuxOverride;
use egui::{Context, Event, Key, RawInput, Rect, Vec2};

mod support;

fn input(event: Event) -> RawInput {
    RawInput {
        screen_rect: Some(Rect::from_min_size(
            egui::Pos2::ZERO,
            Vec2::new(900.0, 700.0),
        )),
        events: vec![event],
        ..RawInput::default()
    }
}

fn context() -> Context {
    let context = Context::default();
    install_icon_fonts(&context);
    context
}

#[test]
fn theme_picker_closes_on_escape() {
    let root = tempfile::tempdir().unwrap();
    let mut dialog = ThemePickerDialog::open(&root.path().join("bootty.toml"), None, "Dark");
    let context = context();
    let mut event = None;

    context
        .run_ui(
            input(Event::Key {
                key: Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }),
            |ui| {
                event = dialog.show(
                    ui.ctx(),
                    theme_from_config(&BoottyConfig::default(), AppearanceVariant::Dark),
                );
            },
        )
        .drop_without_applying_deltas();

    assert_eq!(event, Some(ThemePickerEvent::Close));
}

#[test]
fn keybind_help_accepts_prefixed_bindings_for_display() {
    let dialog = KeybindHelpDialog::open(&["performable:cmd+==increase_font_size:1".to_owned()]);
    let debug = format!("{dialog:?}");

    assert!(debug.contains("cmd+="));
    assert!(debug.contains("increase_font_size:1"));
}

#[test]
fn session_picker_activates_the_selected_session() {
    let scope = MuxScope::new(
        SpaceId::from_persistence(1),
        BindingId::from_persistence(10),
    );
    let session = MuxSession {
        id: "session-1".to_owned(),
        name: "work".to_owned(),
        active: false,
        anchor: MuxPaneAnchor {
            session_id: "session-1".to_owned(),
            process: Some("zsh".to_owned()),
            ..MuxPaneAnchor::default()
        },
        active_window_id: None,
        tag: MuxSessionTag::default(),
        windows: Vec::new(),
    };
    let groups = [BindingSessionGroup {
        scope,
        label: "Local".to_owned(),
        sessions: vec![session],
        selected_session: None,
        active: true,
        can_return_to_last_session: false,
        display_names: HashMap::new(),
    }];
    let mut dialog = SessionPickerDialog::open();
    let context = context();
    let mut event = None;

    context
        .run_ui(
            input(Event::Key {
                key: Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }),
            |ui| {
                event = dialog.show(
                    ui.ctx(),
                    theme_from_config(&BoottyConfig::default(), AppearanceVariant::Dark),
                    &groups,
                );
            },
        )
        .drop_without_applying_deltas();

    assert_eq!(
        event,
        Some(SessionPickerEvent::ActivateSession(
            groups[0].target(&groups[0].sessions[0])
        ))
    );
}

#[test]
fn ditch_dialog_defaults_to_safe_session_close_outside_git() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().to_string_lossy().into_owned();
    let mut dialog = DitchSessionDialog::open("session-1".to_owned(), Some(cwd.clone()));
    let context = context();
    let mut event = None;

    context
        .run_ui(
            input(Event::Key {
                key: Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }),
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
        Some(DitchSessionEvent::Ditch {
            session_id: "session-1".to_owned(),
            cwd: Some(cwd),
            action: DitchAction::KillOnly,
        })
    );
}

#[test]
fn space_editor_closes_on_escape() {
    let mut dialog = SpaceEditorDialog::new_space("folder".to_owned(), SpaceMuxOverride::default());
    let context = context();
    let mut intent = None;

    context
        .run_ui(
            input(Event::Key {
                key: Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }),
            |ui| {
                intent = dialog.show(
                    ui.ctx(),
                    theme_from_config(&BoottyConfig::default(), AppearanceVariant::Dark),
                );
            },
        )
        .drop_without_applying_deltas();

    assert_eq!(intent, Some(SpaceEditorIntent::Close));
}

#[test]
fn opening_a_modal_replaces_the_previous_modal() {
    let root = tempfile::tempdir().unwrap();
    let config = BoottyConfig {
        config_path: root.path().join("config.toml"),
        ..BoottyConfig::default()
    };
    let mut state = AppState::new(
        config,
        support::backends(),
        std::sync::Arc::new(|| {}),
        None,
        None,
    )
    .unwrap();

    assert!(state.open_session_picker_dialog_from_ui());
    assert!(state.open_create_space_dialog_from_ui());

    assert!(matches!(
        state.modal_dialog(),
        Some(ModalDialog::SpaceEditor(_))
    ));
}
