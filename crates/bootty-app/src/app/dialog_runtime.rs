use crate::ui::{
    command_palette::CommandPaletteDialog,
    ditch::DitchSessionDialog,
    keybind_help::KeybindHelpDialog,
    new_session_picker::NewMuxSessionDialog,
    rename::{RenameSessionDialog, RenameTabDialog},
    session_picker::SessionPickerDialog,
    space::SpaceEditorDialog,
    theme_picker::ThemePickerDialog,
};

/// The one floating modal owned by an application window.
pub(super) enum ModalDialog {
    NewSession(NewMuxSessionDialog),
    SpaceEditor(SpaceEditorDialog),
    SessionPicker(SessionPickerDialog),
    RenameSession(RenameSessionDialog),
    RenameTab(RenameTabDialog),
    DitchSession(DitchSessionDialog),
    KeybindHelp(KeybindHelpDialog),
    CommandPalette(CommandPaletteDialog),
    ThemePicker(ThemePickerDialog),
}

/// Defines a taker that removes the open modal only when it holds the named dialog variant.
macro_rules! take_dialog {
    ($name:ident, $variant:ident, $ty:ty) => {
        pub(super) fn $name(&mut self) -> Option<$ty> {
            if !matches!(self.modal.as_deref(), Some(ModalDialog::$variant(_))) {
                return None;
            }
            match *self.modal.take()? {
                ModalDialog::$variant(dialog) => Some(dialog),
                _ => unreachable!("modal variant was checked"),
            }
        }
    };
}

#[derive(Default)]
pub(super) struct DialogRuntime {
    modal: Option<Box<ModalDialog>>,
}

impl DialogRuntime {
    pub(super) fn open(&mut self, dialog: ModalDialog) {
        self.modal = Some(Box::new(dialog));
    }

    pub(super) fn clear(&mut self) {
        self.modal = None;
    }

    pub(super) fn has_modal(&self) -> bool {
        self.modal.is_some()
    }

    pub(super) fn is_session_picker(&self) -> bool {
        matches!(self.modal.as_deref(), Some(ModalDialog::SessionPicker(_)))
    }

    pub(super) fn is_command_palette(&self) -> bool {
        matches!(self.modal.as_deref(), Some(ModalDialog::CommandPalette(_)))
    }

    pub(super) fn is_theme_picker(&self) -> bool {
        matches!(self.modal.as_deref(), Some(ModalDialog::ThemePicker(_)))
    }

    pub(super) fn command_palette(&self) -> Option<&CommandPaletteDialog> {
        match self.modal.as_deref() {
            Some(ModalDialog::CommandPalette(dialog)) => Some(dialog),
            _ => None,
        }
    }

    pub(super) fn clear_space_context(&mut self) {
        if matches!(
            self.modal.as_deref(),
            Some(
                ModalDialog::NewSession(_)
                    | ModalDialog::SpaceEditor(_)
                    | ModalDialog::SessionPicker(_)
                    | ModalDialog::RenameSession(_)
                    | ModalDialog::RenameTab(_)
                    | ModalDialog::DitchSession(_)
            )
        ) {
            self.modal = None;
        }
    }

    take_dialog!(take_new_session, NewSession, NewMuxSessionDialog);
    take_dialog!(take_space_editor, SpaceEditor, SpaceEditorDialog);
    take_dialog!(take_session_picker, SessionPicker, SessionPickerDialog);
    take_dialog!(take_rename_session, RenameSession, RenameSessionDialog);
    take_dialog!(take_rename_tab, RenameTab, RenameTabDialog);
    take_dialog!(take_ditch_session, DitchSession, DitchSessionDialog);
    take_dialog!(take_keybind_help, KeybindHelp, KeybindHelpDialog);
    take_dialog!(take_command_palette, CommandPalette, CommandPaletteDialog);
    take_dialog!(take_theme_picker, ThemePicker, ThemePickerDialog);
}
