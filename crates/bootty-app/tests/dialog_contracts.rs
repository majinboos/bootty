use std::{collections::HashMap, fs};

use bootty_app::{
    app::{AppState, ModalDialog},
    config::{AppearanceVariant, BoottyConfig},
    mux::{
        controller::{BindingId, MuxScope, SpaceId},
        snapshot::{MuxPaneAnchor, MuxSession},
    },
    theme::theme_from_config,
    ui::{
        ditch::{DitchAction, DitchSessionDialog, DitchSessionEvent},
        icons::install_icon_fonts,
        session_navigation::BindingSessionGroup,
        session_picker::{SessionPickerDialog, SessionPickerEvent},
        space::{SpaceEditorDialog, SpaceEditorEvent},
        theme_picker::{ThemePickerDialog, ThemePickerEvent, available_themes},
    },
    workspace::SpaceMuxOverride,
};
use egui::{Context, Event, Key, RawInput, Rect, Vec2};

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
fn theme_catalog_combines_builtin_and_user_themes() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("bootty.toml");
    let themes = root.path().join("themes");
    fs::create_dir(&themes).unwrap();
    fs::write(themes.join("My Theme.toml"), "").unwrap();
    fs::write(themes.join("ignored.txt"), "").unwrap();

    let names = available_themes(&config_path);

    assert!(names.iter().any(|name| name == "My Theme"));
    assert!(!names.iter().any(|name| name == "ignored"));
    assert!(
        names.len() > 1,
        "the builtin theme catalog must remain present"
    );
}

#[test]
fn theme_picker_closes_on_escape() {
    let root = tempfile::tempdir().unwrap();
    let mut dialog = ThemePickerDialog::open(&root.path().join("bootty.toml"), None, "Dark");
    let context = context();
    let mut event = ThemePickerEvent::None;

    let _ = context.run_ui(
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
    );

    assert_eq!(event, ThemePickerEvent::Close);
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
    let mut event = SessionPickerEvent::None;

    let _ = context.run_ui(
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
    );

    assert_eq!(
        event,
        SessionPickerEvent::ActivateSession(groups[0].target(&groups[0].sessions[0]))
    );
}

#[test]
fn ditch_dialog_defaults_to_safe_session_close_outside_git() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().to_string_lossy().into_owned();
    let mut dialog = DitchSessionDialog::open("session-1".to_owned(), Some(cwd.clone()));
    let context = context();
    let mut event = DitchSessionEvent::None;

    let _ = context.run_ui(
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
    );

    assert_eq!(
        event,
        DitchSessionEvent::Ditch {
            session_id: "session-1".to_owned(),
            cwd: Some(cwd),
            action: DitchAction::KillOnly,
        }
    );
}

#[test]
fn space_editor_closes_on_escape() {
    let mut dialog = SpaceEditorDialog::new_space("folder".to_owned(), SpaceMuxOverride::default());
    let context = context();
    let mut event = SpaceEditorEvent::None;

    let _ = context.run_ui(
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
    );

    assert_eq!(event, SpaceEditorEvent::Close);
}

#[test]
fn opening_a_modal_replaces_the_previous_modal() {
    let root = tempfile::tempdir().unwrap();
    let config = BoottyConfig {
        config_path: root.path().join("config.toml"),
        ..BoottyConfig::default()
    };
    let mut state = AppState::new(config, std::sync::Arc::new(|| {}), None, None).unwrap();

    assert!(state.open_session_picker_dialog_from_ui());
    assert!(state.open_create_space_dialog_from_ui());

    assert!(matches!(
        state.take_modal_dialog(),
        Some(ModalDialog::SpaceEditor(_))
    ));
}
