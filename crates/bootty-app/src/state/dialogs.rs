use bootty_command::Caller;
use bootty_config::config::AppearanceVariant;
use bootty_mux::controller::SpaceId;
use bootty_workspace::SpaceMuxOverride;

use super::ditch::{DitchCleanupOutcome, run_ditch_cleanup};
use super::{AppEffect, AppState};
use crate::commands::command_invocation_from_catalog;
use crate::input::focus::InputFocus;
use crate::ui::ModalDialog;
use crate::ui::command_palette::{CommandPaletteDialog, CommandPaletteEvent};
use crate::ui::ditch::{DitchSessionDialog, DitchSessionEvent};
use crate::ui::keybind_help::{KeybindHelpDialog, KeybindHelpEvent};
use crate::ui::new_session_picker::{NewMuxSessionDialog, NewSessionPickerEvent};
use crate::ui::rename::{RenameSessionDialog, RenameSessionEvent, RenameTabDialog, RenameTabEvent};
use crate::ui::session_picker::{SessionPickerDialog, SessionPickerEvent};
use crate::ui::space::{SpaceEditorDialog, SpaceEditorEvent, default_space_icon};
use crate::ui::theme_picker::{ThemePickerDialog, ThemePickerEvent};
use crate::workspace_runtime::RenameSessionOutcome;
impl AppState {
    pub fn take_modal_dialog(&mut self) -> Option<ModalDialog> {
        self.dialogs.take()
    }
    pub fn apply_space_editor_event(&mut self, dialog: SpaceEditorDialog, event: SpaceEditorEvent) {
        match event {
            SpaceEditorEvent::None => self.dialogs.open(ModalDialog::SpaceEditor(dialog)),
            SpaceEditorEvent::Close => self.input_focus = InputFocus::Terminal,
            SpaceEditorEvent::Save {
                space_id,
                name,
                icon,
                color,
                tint_sidebar,
                mux,
            } => {
                let saved = match space_id {
                    Some(space_id) => self.update_space_from_ui(
                        space_id,
                        &name,
                        &icon,
                        color,
                        tint_sidebar,
                        mux.clone(),
                    ),
                    None => self.create_space_with_backend_from_ui(
                        &name,
                        &icon,
                        color,
                        tint_sidebar,
                        mux,
                    ),
                };
                if !saved {
                    self.dialogs.open(ModalDialog::SpaceEditor(dialog));
                }
            }
        }
    }
    pub fn apply_session_picker_event(
        &mut self,
        dialog: SessionPickerDialog,
        event: SessionPickerEvent,
    ) {
        match event {
            SessionPickerEvent::None => {
                self.dialogs.open(ModalDialog::SessionPicker(dialog));
            }
            SessionPickerEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
            SessionPickerEvent::ActivateSession(target) => {
                self.input_focus = InputFocus::Terminal;
                if let Err(error) = self
                    .workspace
                    .add_session_to_binding(target.scope, &target.session_id)
                {
                    self.last_error = Some(error.to_string());
                    return;
                }
                self.activate_scoped_session_from_ui(&target);
            }
        }
    }
    pub fn apply_rename_session_event(
        &mut self,
        dialog: RenameSessionDialog,
        event: RenameSessionEvent,
    ) {
        match event {
            RenameSessionEvent::None => {
                self.dialogs.open(ModalDialog::RenameSession(dialog));
            }
            RenameSessionEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
            RenameSessionEvent::Rename { session_id, name } => {
                let name = name.trim().to_owned();
                if name.is_empty() {
                    self.last_error = Some("session name cannot be empty".to_owned());
                    self.dialogs.open(ModalDialog::RenameSession(dialog));
                    return;
                }
                match self
                    .workspace
                    .rename_active_session(&session_id, &name, &self.repaint)
                {
                    Ok(RenameSessionOutcome::Missing | RenameSessionOutcome::Started) => {}
                    Ok(RenameSessionOutcome::Pending) => {
                        self.dialogs.open(ModalDialog::RenameSession(dialog));
                        return;
                    }
                    Err(error) => {
                        self.last_error = Some(error.to_string());
                        self.dialogs.open(ModalDialog::RenameSession(dialog));
                        return;
                    }
                }
                self.input_focus = InputFocus::Terminal;
            }
        }
    }
    pub fn apply_rename_tab_event(&mut self, dialog: RenameTabDialog, event: RenameTabEvent) {
        match event {
            RenameTabEvent::None => {
                self.dialogs.open(ModalDialog::RenameTab(dialog));
            }
            RenameTabEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
            RenameTabEvent::Rename {
                session_id,
                window_id,
                name,
            } => {
                let name = name.trim();
                self.workspace.active.binding.set_custom_window_name(
                    &session_id,
                    &window_id,
                    name,
                    &self.repaint,
                );
                self.input_focus = InputFocus::Terminal;
            }
        }
    }
    pub fn apply_ditch_session_event(
        &mut self,
        dialog: DitchSessionDialog,
        event: DitchSessionEvent,
    ) {
        match event {
            DitchSessionEvent::None => {
                self.dialogs.open(ModalDialog::DitchSession(dialog));
            }
            DitchSessionEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
            DitchSessionEvent::Ditch {
                session_id,
                cwd,
                action,
            } => {
                let prepared = match self.prepare_ditch_session_command(session_id) {
                    Ok(prepared) => prepared,
                    Err(_) => {
                        self.dialogs.open(ModalDialog::DitchSession(dialog));
                        return;
                    }
                };
                match run_ditch_cleanup(cwd.as_deref(), &action) {
                    DitchCleanupOutcome::NoAction(error) => {
                        // Git cleanup failed before any destructive action. Keep the session alive.
                        self.last_error = Some(format!("ditch: {error}"));
                        self.workspace
                            .defer_binding_membership_reconciliation(prepared.0);
                        self.dialogs.open(ModalDialog::DitchSession(dialog));
                        return;
                    }
                    DitchCleanupOutcome::Partial { branch, error } => {
                        self.last_error = Some(format!(
                            "ditch warning: worktree removed; branch '{branch}' remains: {error}"
                        ));
                    }
                    DitchCleanupOutcome::Complete => {}
                }
                self.submit_prepared_ditch_session_command(prepared);
                self.input_focus = InputFocus::Terminal;
            }
        }
    }
    pub fn apply_keybind_help_event(&mut self, dialog: KeybindHelpDialog, event: KeybindHelpEvent) {
        match event {
            KeybindHelpEvent::None => {
                self.dialogs.open(ModalDialog::KeybindHelp(dialog));
            }
            KeybindHelpEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
        }
    }
    pub fn apply_command_palette_event(
        &mut self,
        dialog: CommandPaletteDialog,
        event: CommandPaletteEvent,
    ) {
        match event {
            CommandPaletteEvent::None => {
                self.dialogs.open(ModalDialog::CommandPalette(dialog));
            }
            CommandPaletteEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
            CommandPaletteEvent::Run(command) => {
                // Resolve the user's current context before another queued caller can change it.
                self.input_focus = InputFocus::Terminal;
                let Some(mut invocation) =
                    command_invocation_from_catalog(command, Caller::CommandPalette)
                else {
                    return;
                };
                if let Some(kind) = self
                    .commands
                    .catalog()
                    .describe(&invocation.command)
                    .and_then(|descriptor| descriptor.target)
                {
                    let Some(target) = self.current_command_target_for(&invocation.command, kind)
                    else {
                        self.commands.clear_queue();
                        self.last_error = Some(format!("no current {kind:?} target is available"));
                        return;
                    };
                    invocation.target = Some(target);
                }
                self.commands.queue(invocation);
            }
        }
    }
    pub fn apply_theme_picker_event(
        &mut self,
        dialog: ThemePickerDialog,
        event: ThemePickerEvent,
        effects: &mut Vec<AppEffect>,
    ) {
        match event {
            ThemePickerEvent::None => {
                self.dialogs.open(ModalDialog::ThemePicker(dialog));
            }
            ThemePickerEvent::Close => {
                self.input_focus = InputFocus::Terminal;
                if self.restore_theme_picker_preview() {
                    effects.push(AppEffect::RequestRepaint);
                }
                self.theme_picker_restore_config = None;
            }
            ThemePickerEvent::RestorePreview => {
                if self.restore_theme_picker_preview() {
                    effects.push(AppEffect::RequestRepaint);
                }
                self.dialogs.open(ModalDialog::ThemePicker(dialog));
            }
            ThemePickerEvent::Preview(theme) => {
                self.preview_active_theme(&theme, effects);
                self.dialogs.open(ModalDialog::ThemePicker(dialog));
            }
            ThemePickerEvent::Select(theme) => {
                self.input_focus = InputFocus::Terminal;
                self.theme_picker_restore_config = None;
                self.persist_active_theme(&theme, effects);
            }
        }
    }
    pub fn apply_picker_event(
        &mut self,
        dialog: NewMuxSessionDialog,
        event: NewSessionPickerEvent,
    ) {
        match event {
            NewSessionPickerEvent::None => {
                self.dialogs.open(ModalDialog::NewSession(dialog));
            }
            NewSessionPickerEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
            NewSessionPickerEvent::Error(error) => {
                self.last_error = Some(error);
                self.dialogs.open(ModalDialog::NewSession(dialog));
            }
            NewSessionPickerEvent::CreateWorktree { repo, branch } => {
                match bootty_mux::project::add_worktree(&repo, &branch) {
                    Ok(path) => {
                        self.create_project_session_for_cwd(path);
                        self.input_focus = InputFocus::Terminal;
                    }
                    Err(error) => {
                        self.last_error = Some(format!("worktree: {error}"));
                        self.dialogs.open(ModalDialog::NewSession(dialog));
                    }
                }
            }
            NewSessionPickerEvent::CreateSession { cwd } => {
                self.create_project_session_for_cwd(cwd);
                self.input_focus = InputFocus::Terminal;
            }
        }
    }
    pub(super) fn close_overlay_dialogs(&mut self) -> bool {
        let restored_preview = self.restore_theme_picker_preview();
        self.theme_picker_restore_config = None;
        self.dialogs.clear();
        let outcome = self
            .terminal_interaction
            .close_overlay_dialogs(&mut self.workspace.active.binding.terminal);
        if let Some(error) = outcome.last_error {
            self.last_error = Some(error);
        }
        self.apply_terminal_focus_intent(outcome.focus_intent);
        restored_preview
    }
    pub(super) fn open_new_mux_session_dialog(&mut self) {
        self.close_overlay_dialogs();
        self.dialogs.open(ModalDialog::NewSession(
            self.active_multiplexer()
                .remote
                .clone()
                .map(|remote| NewMuxSessionDialog::open_remote(remote, self.repaint.clone()))
                .unwrap_or_else(NewMuxSessionDialog::open),
        ));
        self.input_focus = InputFocus::Picker;
    }
    pub fn open_create_space_dialog_from_ui(&mut self) -> bool {
        self.close_overlay_dialogs();
        let existing_icons = self
            .space_summaries()
            .into_iter()
            .map(|space| space.icon)
            .collect::<Vec<_>>();
        let profiles = self
            .config()
            .ssh_profiles
            .iter()
            .map(|(id, profile)| (id.clone(), profile.clone()))
            .collect::<Vec<_>>();
        self.dialogs.open(ModalDialog::SpaceEditor(
            SpaceEditorDialog::new_space(
                default_space_icon(&existing_icons),
                SpaceMuxOverride::default(),
            )
            .with_profiles(profiles.into_iter()),
        ));
        self.input_focus = InputFocus::Picker;
        true
    }
    pub fn open_edit_space_dialog_from_ui(&mut self, space_id: SpaceId) -> bool {
        let placement = self.workspace.space_placement(space_id);
        let Some((space, placement)) = self
            .space_summaries()
            .into_iter()
            .find(|space| space.id == space_id)
            .zip(placement)
        else {
            return false;
        };
        self.close_overlay_dialogs();
        let profiles = self
            .config()
            .ssh_profiles
            .iter()
            .map(|(id, profile)| (id.clone(), profile.clone()))
            .collect::<Vec<_>>();
        self.dialogs.open(ModalDialog::SpaceEditor(
            SpaceEditorDialog::edit_space(
                space.id,
                space.name,
                space.icon,
                space.color,
                space.tint_sidebar,
                placement,
            )
            .with_profiles(profiles.into_iter()),
        ));
        self.input_focus = InputFocus::Picker;
        true
    }
    pub fn open_new_session_dialog_from_ui(&mut self) -> bool {
        self.open_new_mux_session_dialog();
        true
    }
    pub(super) fn open_session_picker_dialog(&mut self) {
        self.close_overlay_dialogs();
        self.dialogs
            .open(ModalDialog::SessionPicker(SessionPickerDialog::open()));
        self.input_focus = InputFocus::Picker;
    }
    pub fn open_session_picker_dialog_from_ui(&mut self) -> bool {
        self.open_session_picker_dialog();
        true
    }
    pub(super) fn toggle_session_picker_dialog(&mut self) {
        if self.dialogs.is_session_picker() {
            self.dialogs.clear();
            self.input_focus = InputFocus::Terminal;
        } else {
            self.open_session_picker_dialog();
        }
    }
    pub(super) fn open_rename_session_dialog(&mut self) {
        let Some(selected) = self
            .workspace
            .active
            .binding
            .mux
            .selected_session()
            .map(str::to_owned)
        else {
            return;
        };
        self.open_rename_session_dialog_for(&selected);
    }
    pub fn open_rename_session_dialog_for(&mut self, session_id: &str) -> bool {
        let Some((session_id, name)) = self
            .workspace
            .active
            .binding
            .mux
            .session_by_id_or_name(session_id)
            .map(|session| {
                // Prefill what bootty shows, so a backend-only uniqueness suffix is not something
                // the user has to delete out of the field.
                let name = self
                    .workspace
                    .active
                    .binding
                    .session_names
                    .display_name(&session.id)
                    .unwrap_or(session.name.as_str())
                    .to_owned();
                (session.id.clone(), name)
            })
        else {
            return false;
        };
        self.close_overlay_dialogs();
        self.dialogs
            .open(ModalDialog::RenameSession(RenameSessionDialog::open(
                session_id, name,
            )));
        self.input_focus = InputFocus::Picker;
        true
    }
    pub(super) fn open_rename_tab_dialog(&mut self) {
        let Some((session_id, window_id, _)) = self.selected_window_for_rename() else {
            return;
        };
        self.open_rename_tab_dialog_for(&session_id, &window_id);
    }
    pub fn open_rename_tab_dialog_for(&mut self, session_id: &str, window_id: &str) -> bool {
        let Some((session_id, window_id, name)) = self
            .workspace
            .active
            .binding
            .mux
            .session_by_id_or_name(session_id)
            .and_then(|session| {
                session
                    .windows
                    .iter()
                    .find(|window| window.id == window_id)
                    .map(|window| (session.id.clone(), window.id.clone(), window.name.clone()))
            })
        else {
            return false;
        };
        self.close_overlay_dialogs();
        self.dialogs
            .open(ModalDialog::RenameTab(RenameTabDialog::open(
                session_id, window_id, name,
            )));
        self.input_focus = InputFocus::Picker;
        true
    }
    fn selected_window_for_rename(&self) -> Option<(String, String, String)> {
        let selected = self.workspace.active.binding.mux.selected_session()?;
        let session = self
            .workspace
            .active
            .binding
            .mux
            .session_by_id_or_name(selected)?;
        let window_id = self
            .workspace
            .active
            .binding
            .mux
            .selected_window()
            .or(session.active_window_id.as_deref());
        let window = window_id
            .and_then(|id| session.windows.iter().find(|window| window.id == id))
            .or_else(|| session.windows.first())?;
        Some((session.id.clone(), window.id.clone(), window.name.clone()))
    }
    pub(super) fn open_ditch_session_dialog(&mut self) {
        let Some(selected) = self
            .workspace
            .active
            .binding
            .mux
            .selected_session()
            .map(str::to_owned)
        else {
            return;
        };
        self.open_ditch_session_dialog_for(&selected);
    }
    pub fn open_ditch_session_dialog_for(&mut self, session_id: &str) -> bool {
        let Some((session_id, cwd)) = self
            .workspace
            .active
            .binding
            .mux
            .session_by_id_or_name(session_id)
            .map(|session| (session.id.clone(), session.anchor.cwd.clone()))
        else {
            return false;
        };
        self.close_overlay_dialogs();
        self.dialogs
            .open(ModalDialog::DitchSession(DitchSessionDialog::open(
                session_id, cwd,
            )));
        self.input_focus = InputFocus::Picker;
        true
    }
    pub(super) fn open_keybind_help_dialog(&mut self) {
        let bindings = self
            .config()
            .input
            .keybinds_for_backend(self.workspace.active.binding.multiplexer.backend);
        self.close_overlay_dialogs();
        self.dialogs
            .open(ModalDialog::KeybindHelp(KeybindHelpDialog::open(&bindings)));
        self.input_focus = InputFocus::Picker;
    }
    pub(super) fn open_command_palette_dialog(&mut self) {
        let bindings = self
            .config()
            .input
            .keybinds_for_backend(self.workspace.active.binding.multiplexer.backend);
        self.close_overlay_dialogs();
        self.dialogs
            .open(ModalDialog::CommandPalette(CommandPaletteDialog::open(
                &bindings,
            )));
        self.input_focus = InputFocus::Picker;
    }
    pub(super) fn open_theme_picker_dialog(&mut self) {
        let config = self.config();
        let branch = match self.active_appearance_variant {
            AppearanceVariant::Light => "Light appearance",
            AppearanceVariant::Dark => "Dark appearance",
        };
        let current = config
            .theme_for_appearance(self.active_appearance_variant)
            .map(str::to_owned);
        let config_path = config.config_path.clone();
        let restore_config = config.clone();
        self.close_overlay_dialogs();
        self.theme_picker_restore_config = Some(restore_config);
        self.dialogs
            .open(ModalDialog::ThemePicker(ThemePickerDialog::open(
                &config_path,
                current.as_deref(),
                branch,
            )));
        self.input_focus = InputFocus::Picker;
    }
}
