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
pub enum ModalDialog {
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

    pub(super) fn take(&mut self) -> Option<ModalDialog> {
        self.modal.take().map(|dialog| *dialog)
    }
}
