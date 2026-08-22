use crate::ui::{
    command_palette::CommandPaletteDialog,
    ditch::DitchSessionDialog,
    keybind_help::KeybindHelpDialog,
    new_session_picker::NewMuxSessionDialog,
    rename::{RenameSessionDialog, RenameTabDialog},
    session_picker::SessionPickerDialog,
    space::SpaceEditorDialog,
    space_picker::SpacePickerDialog,
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
    SpacePicker(SpacePickerDialog),
}

#[derive(Default)]
pub(crate) struct DialogRuntime {
    modal: Option<Box<ModalDialog>>,
}

impl DialogRuntime {
    pub(crate) fn open(&mut self, dialog: ModalDialog) {
        self.modal = Some(Box::new(dialog));
    }

    pub(crate) fn clear(&mut self) {
        self.modal = None;
    }

    pub(crate) fn has_modal(&self) -> bool {
        self.modal.is_some()
    }

    pub(crate) fn current(&self) -> Option<&ModalDialog> {
        self.modal.as_deref()
    }

    pub(crate) fn current_mut(&mut self) -> Option<&mut ModalDialog> {
        self.modal.as_deref_mut()
    }

    pub(crate) fn is_session_picker(&self) -> bool {
        matches!(self.modal.as_deref(), Some(ModalDialog::SessionPicker(_)))
    }

    pub(crate) fn is_command_palette(&self) -> bool {
        matches!(self.modal.as_deref(), Some(ModalDialog::CommandPalette(_)))
    }

    pub(crate) fn is_theme_picker(&self) -> bool {
        matches!(self.modal.as_deref(), Some(ModalDialog::ThemePicker(_)))
    }

    pub(crate) fn command_palette(&self) -> Option<&CommandPaletteDialog> {
        match self.modal.as_deref() {
            Some(ModalDialog::CommandPalette(dialog)) => Some(dialog),
            _ => None,
        }
    }

    pub(crate) fn clear_space_context(&mut self) {
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
}
