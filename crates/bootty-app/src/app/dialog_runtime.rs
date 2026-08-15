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

    pub(super) fn take_new_session(&mut self) -> Option<NewMuxSessionDialog> {
        if !matches!(self.modal.as_deref(), Some(ModalDialog::NewSession(_))) {
            return None;
        }
        match *self.modal.take()? {
            ModalDialog::NewSession(dialog) => Some(dialog),
            _ => unreachable!("modal variant was checked"),
        }
    }

    pub(super) fn take_space_editor(&mut self) -> Option<SpaceEditorDialog> {
        if !matches!(self.modal.as_deref(), Some(ModalDialog::SpaceEditor(_))) {
            return None;
        }
        match *self.modal.take()? {
            ModalDialog::SpaceEditor(dialog) => Some(dialog),
            _ => unreachable!("modal variant was checked"),
        }
    }

    pub(super) fn take_session_picker(&mut self) -> Option<SessionPickerDialog> {
        if !matches!(self.modal.as_deref(), Some(ModalDialog::SessionPicker(_))) {
            return None;
        }
        match *self.modal.take()? {
            ModalDialog::SessionPicker(dialog) => Some(dialog),
            _ => unreachable!("modal variant was checked"),
        }
    }

    pub(super) fn take_rename_session(&mut self) -> Option<RenameSessionDialog> {
        if !matches!(self.modal.as_deref(), Some(ModalDialog::RenameSession(_))) {
            return None;
        }
        match *self.modal.take()? {
            ModalDialog::RenameSession(dialog) => Some(dialog),
            _ => unreachable!("modal variant was checked"),
        }
    }

    pub(super) fn take_rename_tab(&mut self) -> Option<RenameTabDialog> {
        if !matches!(self.modal.as_deref(), Some(ModalDialog::RenameTab(_))) {
            return None;
        }
        match *self.modal.take()? {
            ModalDialog::RenameTab(dialog) => Some(dialog),
            _ => unreachable!("modal variant was checked"),
        }
    }

    pub(super) fn take_ditch_session(&mut self) -> Option<DitchSessionDialog> {
        if !matches!(self.modal.as_deref(), Some(ModalDialog::DitchSession(_))) {
            return None;
        }
        match *self.modal.take()? {
            ModalDialog::DitchSession(dialog) => Some(dialog),
            _ => unreachable!("modal variant was checked"),
        }
    }

    pub(super) fn take_keybind_help(&mut self) -> Option<KeybindHelpDialog> {
        if !matches!(self.modal.as_deref(), Some(ModalDialog::KeybindHelp(_))) {
            return None;
        }
        match *self.modal.take()? {
            ModalDialog::KeybindHelp(dialog) => Some(dialog),
            _ => unreachable!("modal variant was checked"),
        }
    }

    pub(super) fn take_command_palette(&mut self) -> Option<CommandPaletteDialog> {
        if !matches!(self.modal.as_deref(), Some(ModalDialog::CommandPalette(_))) {
            return None;
        }
        match *self.modal.take()? {
            ModalDialog::CommandPalette(dialog) => Some(dialog),
            _ => unreachable!("modal variant was checked"),
        }
    }

    pub(super) fn take_theme_picker(&mut self) -> Option<ThemePickerDialog> {
        if !matches!(self.modal.as_deref(), Some(ModalDialog::ThemePicker(_))) {
            return None;
        }
        match *self.modal.take()? {
            ModalDialog::ThemePicker(dialog) => Some(dialog),
            _ => unreachable!("modal variant was checked"),
        }
    }
}
