use pretty_assertions::assert_eq;

use std::collections::HashMap;

use bootty_app::{
    AppState, ModalDialog,
    theme::theme_from_config,
    ui::{
        ditch::{DitchAction, DitchSessionDialog, DitchSessionEvent},
        new_session_picker::{NewMuxSessionDialog, NewSessionPickerEvent},
        rename::{RenameSessionDialog, RenameSessionEvent, RenameTabDialog, RenameTabEvent},
        session_navigation::BindingSessionGroup,
        session_picker::{SessionPickerDialog, SessionPickerEvent},
        space::{SpaceEditorDialog, SpaceEditorIntent},
        theme_picker::{ThemePickerDialog, ThemePickerEvent},
    },
};
use bootty_config::config::{AppearanceVariant, BoottyConfig};
use bootty_mux::{
    controller::SpaceId,
    snapshot::{MuxPaneAnchor, MuxSession, MuxSessionTag},
};
use bootty_ui::icons::install_icon_fonts;
use bootty_workspace::SpaceMuxOverride;
use egui::{Context, Key, Modifiers, RawInput, Rect, Vec2};

#[path = "support/events.rs"]
mod events;
mod support;

fn press<T>(key: Key, mut show: impl FnMut(&Context) -> Option<T>) -> Option<T> {
    let context = Context::default();
    install_icon_fonts(&context);
    let mut event = None;
    context
        .run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(900.0, 700.0),
                )),
                events: vec![events::key_event(key, Modifiers::NONE)],
                ..RawInput::default()
            },
            |ui| event = show(ui.ctx()),
        )
        .drop_without_applying_deltas();
    event
}

#[test]
fn theme_picker_closes_on_escape() {
    let root = assert_fs::TempDir::new().unwrap();
    let mut dialog = ThemePickerDialog::open(&root.path().join("bootty.toml"), None, "Dark");
    let event = press(Key::Escape, |context| {
        dialog.show(
            context,
            theme_from_config(&BoottyConfig::default(), AppearanceVariant::Dark),
        )
    });

    assert_eq!(event, Some(ThemePickerEvent::Close));
}

#[test]
fn session_picker_activates_the_selected_session() {
    let scope = SpaceId::from_persistence(1);
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
    let event = press(Key::Enter, |context| {
        dialog.show(
            context,
            theme_from_config(&BoottyConfig::default(), AppearanceVariant::Dark),
            &groups,
        )
    });

    assert_eq!(
        event,
        Some(SessionPickerEvent::ActivateSession(
            groups[0].target(&groups[0].sessions[0])
        ))
    );
}

#[test]
fn ditch_dialog_defaults_to_safe_session_close_outside_git() {
    let root = assert_fs::TempDir::new().unwrap();
    let cwd = root.path().to_string_lossy().into_owned();
    let mut dialog = DitchSessionDialog::open("session-1".to_owned(), Some(cwd.clone()));
    let event = press(Key::Enter, |context| {
        dialog.show(
            context,
            theme_from_config(&BoottyConfig::default(), AppearanceVariant::Dark),
        )
    });

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
    let intent = press(Key::Escape, |context| {
        dialog.show(
            context,
            theme_from_config(&BoottyConfig::default(), AppearanceVariant::Dark),
        )
    });

    assert_eq!(intent, Some(SpaceEditorIntent::Close));
}

#[test]
fn opening_a_modal_replaces_the_previous_modal() {
    let root = assert_fs::TempDir::new().unwrap();
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

fn show_new_session(
    dialog: &mut NewMuxSessionDialog,
    input: RawInput,
) -> Option<NewSessionPickerEvent> {
    let context = Context::default();
    install_icon_fonts(&context);
    let mut event = None;
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
    assert_eq!(
        show_new_session(&mut NewMuxSessionDialog::open(), RawInput::default()),
        None
    );
}

#[test]
fn escape_closes_the_new_session_picker() {
    assert_eq!(
        show_new_session(
            &mut NewMuxSessionDialog::open(),
            RawInput {
                events: vec![events::key_event(Key::Escape, Modifiers::NONE)],
                ..RawInput::default()
            },
        ),
        Some(NewSessionPickerEvent::Close)
    );
}

fn submit<T>(context: &Context, mut show: impl FnMut(&Context) -> Option<T>) -> Option<T> {
    context
        .run_ui(RawInput::default(), |ui| {
            let _ = show(ui.ctx());
        })
        .drop_without_applying_deltas();
    let mut event = None;
    context
        .run_ui(
            RawInput {
                events: vec![events::key_event(Key::Enter, Modifiers::NONE)],
                ..RawInput::default()
            },
            |ui| event = show(ui.ctx()),
        )
        .drop_without_applying_deltas();
    event
}

#[test]
fn session_rename_trims_the_submitted_name_and_rejects_blank_names() {
    let context = Context::default();
    install_icon_fonts(&context);
    let mut dialog = RenameSessionDialog::open("session-1".to_owned(), "  review  ".to_owned());
    assert_eq!(
        submit(&context, |context| {
            dialog.show(
                context,
                theme_from_config(&BoottyConfig::default(), AppearanceVariant::Dark),
            )
        }),
        Some(RenameSessionEvent::Rename {
            session_id: "session-1".to_owned(),
            name: "review".to_owned(),
        })
    );

    let mut blank = RenameSessionDialog::open("session-2".to_owned(), "   ".to_owned());
    assert_eq!(
        submit(&context, |context| {
            blank.show(
                context,
                theme_from_config(&BoottyConfig::default(), AppearanceVariant::Dark),
            )
        }),
        None
    );
}

#[test]
fn blank_tab_rename_restores_terminal_managed_titles() {
    let context = Context::default();
    install_icon_fonts(&context);
    let mut dialog = RenameTabDialog::open(
        "session-1".to_owned(),
        "window-1".to_owned(),
        "   ".to_owned(),
    );

    assert_eq!(
        submit(&context, |context| {
            dialog.show(
                context,
                theme_from_config(&BoottyConfig::default(), AppearanceVariant::Dark),
            )
        }),
        Some(RenameTabEvent::Rename {
            session_id: "session-1".to_owned(),
            window_id: "window-1".to_owned(),
            name: String::new(),
        })
    );
}
