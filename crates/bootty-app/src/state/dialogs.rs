use bootty_command::Caller;
use bootty_config::config::{AppearanceVariant, SshProfileConfig};
use bootty_mux::controller::SpaceId;
use bootty_workspace::SpaceMuxOverride;

use super::ditch::{DitchCleanupOutcome, run_ditch_cleanup};
use super::{AppEffect, AppState};
use crate::commands::command_invocation_from_catalog;
use crate::input::focus::InputFocus;
use crate::ui::ModalDialog;
use crate::ui::command_palette::{CommandPaletteDialog, CommandPaletteEvent};
use crate::ui::ditch::{DitchSessionDialog, DitchSessionEvent};
use crate::ui::keybind_help::KeybindHelpDialog;
use crate::ui::new_session_picker::{NewMuxSessionDialog, NewSessionPickerEvent};
use crate::ui::rename::{RenameSessionDialog, RenameSessionEvent, RenameTabDialog, RenameTabEvent};
use crate::ui::session_navigation::ScopedSessionTarget;
use crate::ui::session_picker::{SessionPickerDialog, SessionPickerEvent};
use crate::ui::space::{SpaceEditorDialog, SpaceEditorIntent, default_space_icon};
use crate::ui::space_picker::{SpacePickerDialog, SpacePickerEvent};
use crate::ui::theme_picker::{ThemePickerDialog, ThemePickerEvent};
use crate::workspace_runtime::RenameSessionOutcome;
impl AppState {
    fn show_overlay(&mut self, dialog: ModalDialog) {
        self.dialogs.open(dialog);
        self.input_focus = InputFocus::Picker;
    }

    fn open_overlay(&mut self, dialog: ModalDialog) {
        self.close_overlay_dialogs();
        self.show_overlay(dialog);
    }

    fn ssh_profiles(&self) -> Vec<(String, SshProfileConfig)> {
        self.config()
            .ssh_profiles
            .iter()
            .map(|(id, profile)| (id.clone(), profile.clone()))
            .collect()
    }

    fn selected_session_id(&self) -> Option<String> {
        self.workspace
            .active
            .binding
            .mux
            .selected_session()
            .map(str::to_owned)
    }

    pub fn modal_dialog(&self) -> Option<&ModalDialog> {
        self.dialogs.current()
    }

    pub fn modal_dialog_mut(&mut self) -> Option<&mut ModalDialog> {
        self.dialogs.current_mut()
    }

    fn dismiss_modal_dialog(&mut self) {
        self.dialogs.clear();
        self.input_focus = InputFocus::Terminal;
    }
    pub fn apply_space_editor_intent(&mut self, intent: SpaceEditorIntent) {
        match intent {
            SpaceEditorIntent::Close => self.dismiss_modal_dialog(),
            SpaceEditorIntent::Save(draft) => {
                let mux = SpaceMuxOverride {
                    backend: draft.backend,
                    remote: draft.remote_source,
                };
                let saved = match draft.space_id {
                    Some(space_id) => self.update_space_from_ui(
                        space_id,
                        &draft.name,
                        &draft.icon,
                        draft.color,
                        draft.tint_sidebar,
                        mux,
                    ),
                    None => self.create_space_with_backend_from_ui(
                        &draft.name,
                        &draft.icon,
                        draft.color,
                        draft.tint_sidebar,
                        mux,
                    ),
                };
                if saved {
                    self.dismiss_modal_dialog();
                }
            }
        }
    }
    pub fn apply_space_picker_event(&mut self, event: SpacePickerEvent) {
        match event {
            SpacePickerEvent::Close => self.dismiss_modal_dialog(),
            SpacePickerEvent::Move { session, space } => {
                let moved = match space {
                    Some(space) => self.move_scoped_session_to_space(&session, space),
                    None => self.detach_scoped_session_from_space(&session),
                };
                if moved {
                    self.dismiss_modal_dialog();
                }
            }
        }
    }

    /// Opens the Space picker for a session, or reports why it cannot move.
    pub fn open_space_picker_for(&mut self, target: &ScopedSessionTarget) -> bool {
        let Some(name) = self.session_display_name(target) else {
            self.last_error = Some("this session is no longer available".to_owned());
            return false;
        };
        let spaces = self.session_move_targets(target);
        if spaces.iter().all(|space| space.current) {
            self.last_error = Some("there is nowhere else to move it yet".to_owned());
            return false;
        }
        self.open_overlay(ModalDialog::SpacePicker(SpacePickerDialog::open(
            target.clone(),
            name,
            spaces,
        )));
        true
    }

    pub fn apply_session_picker_event(&mut self, event: SessionPickerEvent) {
        match event {
            SessionPickerEvent::Close => self.dismiss_modal_dialog(),
            SessionPickerEvent::ActivateSession(target) => {
                self.dismiss_modal_dialog();
                if let Err(error) = self.workspace.adopt_session_into_binding(
                    target.scope,
                    &target.session_id,
                    &self.repaint,
                ) {
                    self.last_error = Some(error.to_string());
                    return;
                }
                self.activate_scoped_session_from_ui(&target);
            }
        }
    }
    pub fn apply_rename_session_event(&mut self, event: RenameSessionEvent) {
        match event {
            RenameSessionEvent::Close => self.dismiss_modal_dialog(),
            RenameSessionEvent::Rename { session_id, name } => {
                let name = name.trim().to_owned();
                if name.is_empty() {
                    self.last_error = Some("session name cannot be empty".to_owned());
                    return;
                }
                match self
                    .workspace
                    .rename_active_session(&session_id, &name, &self.repaint)
                {
                    Ok(RenameSessionOutcome::Missing | RenameSessionOutcome::Started) => {}
                    Ok(RenameSessionOutcome::Pending) => return,
                    Err(error) => {
                        self.last_error = Some(error.to_string());
                        return;
                    }
                }
                self.dismiss_modal_dialog();
            }
        }
    }
    pub fn apply_rename_tab_event(&mut self, event: RenameTabEvent) {
        match event {
            RenameTabEvent::Close => self.dismiss_modal_dialog(),
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
                self.dismiss_modal_dialog();
            }
        }
    }
    pub fn apply_ditch_session_event(&mut self, event: DitchSessionEvent) {
        match event {
            DitchSessionEvent::Close => self.dismiss_modal_dialog(),
            DitchSessionEvent::Ditch {
                session_id,
                cwd,
                action,
            } => {
                let prepared = match self.prepare_ditch_session_command(session_id) {
                    Ok(prepared) => prepared,
                    Err(_) => return,
                };
                match run_ditch_cleanup(cwd.as_deref(), &action) {
                    DitchCleanupOutcome::NoAction(error) => {
                        // Git cleanup failed before any destructive action. Keep the session alive.
                        self.last_error = Some(format!("ditch: {error}"));
                        self.workspace
                            .defer_binding_membership_reconciliation(prepared.0);
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
                self.dismiss_modal_dialog();
            }
        }
    }
    pub fn dismiss_keybind_help(&mut self) {
        self.dismiss_modal_dialog();
    }
    pub fn apply_command_palette_event(&mut self, event: CommandPaletteEvent) {
        match event {
            CommandPaletteEvent::Close => self.dismiss_modal_dialog(),
            CommandPaletteEvent::Run(command) => {
                // Resolve the user's current context before another queued caller can change it.
                self.dismiss_modal_dialog();
                let Some(mut invocation) =
                    command_invocation_from_catalog(command, Caller::CommandPalette)
                else {
                    return;
                };
                if let Some(kind) = self.commands.target_kind(&invocation.command) {
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
        event: ThemePickerEvent,
        effects: &mut Vec<AppEffect>,
    ) {
        match event {
            ThemePickerEvent::Close => {
                self.dismiss_modal_dialog();
                if self.restore_theme_picker_preview() {
                    effects.push(AppEffect::RequestRepaint);
                }
                self.theme_picker_restore_config = None;
            }
            ThemePickerEvent::RestorePreview => {
                if self.restore_theme_picker_preview() {
                    effects.push(AppEffect::RequestRepaint);
                }
            }
            ThemePickerEvent::Preview(theme) => {
                self.preview_active_theme(&theme, effects);
            }
            ThemePickerEvent::Select(theme) => {
                self.dismiss_modal_dialog();
                self.theme_picker_restore_config = None;
                self.persist_active_theme(&theme, effects);
            }
        }
    }
    pub fn apply_picker_event(&mut self, event: NewSessionPickerEvent) {
        match event {
            NewSessionPickerEvent::Close => self.dismiss_modal_dialog(),
            NewSessionPickerEvent::Error(error) => {
                self.last_error = Some(error);
            }
            NewSessionPickerEvent::CreateWorktree { repo, branch } => {
                match bootty_mux::project::add_worktree(&repo, &branch) {
                    Ok(path) => {
                        self.create_project_session_for_cwd(path);
                        self.dismiss_modal_dialog();
                    }
                    Err(error) => {
                        self.last_error = Some(format!("worktree: {error}"));
                    }
                }
            }
            NewSessionPickerEvent::CreateSession { cwd } => {
                self.create_project_session_for_cwd(cwd);
                self.dismiss_modal_dialog();
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
        self.apply_terminal_outcome(outcome.last_error, outcome.focus_intent);
        restored_preview
    }
    pub(super) fn open_new_mux_session_dialog(&mut self) {
        self.close_overlay_dialogs();
        self.show_overlay(ModalDialog::NewSession(
            self.active_multiplexer()
                .remote
                .clone()
                .map(|remote| NewMuxSessionDialog::open_remote(remote, self.repaint.clone()))
                .unwrap_or_else(NewMuxSessionDialog::open),
        ));
    }
    pub fn open_create_space_dialog_from_ui(&mut self) -> bool {
        self.close_overlay_dialogs();
        let existing_icons = self
            .space_summaries()
            .into_iter()
            .map(|space| space.icon)
            .collect::<Vec<_>>();
        let profiles = self.ssh_profiles();
        self.show_overlay(ModalDialog::SpaceEditor(
            SpaceEditorDialog::new_space(
                default_space_icon(&existing_icons),
                SpaceMuxOverride::default(),
            )
            .with_profiles(profiles.into_iter()),
        ));
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
        let profiles = self.ssh_profiles();
        self.show_overlay(ModalDialog::SpaceEditor(
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
        true
    }
    pub fn open_new_session_dialog_from_ui(&mut self) -> bool {
        self.open_new_mux_session_dialog();
        true
    }
    pub(super) fn open_session_picker_dialog(&mut self) {
        self.open_overlay(ModalDialog::SessionPicker(SessionPickerDialog::open()));
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
    pub(super) fn open_space_picker_for_current_session(&mut self) -> bool {
        let Some(selected) = self.selected_session_id() else {
            return false;
        };
        let target = ScopedSessionTarget::new(self.workspace.active.binding.scope, selected);
        self.open_space_picker_for(&target)
    }

    pub(super) fn open_rename_session_dialog(&mut self) {
        let Some(selected) = self.selected_session_id() else {
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
                let name = session
                    .tag
                    .identity
                    .as_deref()
                    .and_then(|identity| self.workspace.active.binding.sessions.get(identity))
                    .map_or(session.name.as_str(), |claimed| claimed.label())
                    .to_owned();
                (session.id.clone(), name)
            })
        else {
            return false;
        };
        self.open_overlay(ModalDialog::RenameSession(RenameSessionDialog::open(
            session_id, name,
        )));
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
        self.open_overlay(ModalDialog::RenameTab(RenameTabDialog::open(
            session_id, window_id, name,
        )));
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
        let Some(selected) = self.selected_session_id() else {
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
        self.open_overlay(ModalDialog::DitchSession(DitchSessionDialog::open(
            session_id, cwd,
        )));
        true
    }
    pub(super) fn open_keybind_help_dialog(&mut self) {
        let bindings = self
            .config()
            .input
            .keybinds_for_backend(self.workspace.active.binding.multiplexer.backend);
        self.open_overlay(ModalDialog::KeybindHelp(KeybindHelpDialog::open(&bindings)));
    }
    pub(super) fn open_command_palette_dialog(&mut self) {
        let bindings = self
            .config()
            .input
            .keybinds_for_backend(self.workspace.active.binding.multiplexer.backend);
        self.open_overlay(ModalDialog::CommandPalette(CommandPaletteDialog::open(
            &bindings,
        )));
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
        self.show_overlay(ModalDialog::ThemePicker(ThemePickerDialog::open(
            &config_path,
            current.as_deref(),
            branch,
        )));
    }
}
