use crate::terminal_config::terminal_text_config;

use std::{path::PathBuf, sync::mpsc, time::Instant};

use anyhow::Result;
use bootty_command::{BoundAppCommandSender, Caller};
use bootty_config::config::{AppearanceVariant, BoottyConfig};
use bootty_control::ControlPlane;
use bootty_extension::{
    ExtensionHost, ExtensionUiAction, ModuleItem, PublishedSurfaceSnapshot, SurfacePlacement,
};
use bootty_winit::direct_input::{
    DirectKeyInput, ModifierSideState, suppress_egui_events_for_direct_input,
};
use eframe::egui;

use crate::error_catalog::{self, ErrorNotice};
use crate::renderer::{TerminalWorkspaceView, animate_indeterminate_progress};
use crate::state::{AppEffect, AppState, FrameInputs, ViewportSnapshot};
use crate::ui::chrome::ChromeRuntime;

use crate::{
    menu::AppMenu,
    theme::theme_tokens,
    ui::{
        ModalDialog,
        settings::{SettingsAction, SettingsSurface},
    },
};

pub struct BoottyApp {
    state: AppState,
    terminal_view: TerminalWorkspaceView,
    chrome: ChromeRuntime,
    settings: SettingsSurface,
    error_details_open: bool,
    error_toast_identity: Option<String>,
    error_toast_message: Option<String>,
    error_toast_started: Option<Instant>,
    // Held for the process lifetime so the native menu stays installed.
    _menu: Option<AppMenu>,
    extensions: ExtensionHost,
    extension_theme: Vec<(String, String)>,
    /// Whether the window had keyboard focus this frame. Extension hosts throttle themselves while
    /// it is false, so an unfocused window stops animating (and repainting) its chrome.
    window_focused: bool,
    /// Filter and selection state for each module-declared floating window.
    extension_windows: crate::ui::extension_window::ExtensionWindows,
}

impl BoottyApp {
    pub(crate) fn new_for_native_host(
        cc: &eframe::CreationContext<'_>,
        config: BoottyConfig,
        window_state_key: String,
        backends: std::sync::Arc<bootty_mux::provider::MuxBackendRegistry>,
        direct_input_rx: mpsc::Receiver<DirectKeyInput>,
        modifier_side_rx: mpsc::Receiver<ModifierSideState>,
        control_plane: ControlPlane,
    ) -> Result<Self> {
        configure_egui_fonts(&cc.egui_ctx, config.font.ui_families());
        let repaint_ctx = cc.egui_ctx.clone();
        let repaint: bootty_mux::RepaintHandle =
            std::sync::Arc::new(move || repaint_ctx.request_repaint());
        let text_config = terminal_text_config(&config.font);
        let target_format = cc
            .wgpu_render_state
            .as_ref()
            .map(|render_state| render_state.target_format);
        let terminal_view = TerminalWorkspaceView::new(target_format, text_config);

        // User extensions live beside the config file. Built-ins are Luau modules;
        // user `.lua` / `.luau` files override same-named defaults per extension surface.
        let startup_variant = config.appearance.mode.variant(AppearanceVariant::Dark);
        let extension_theme = theme_tokens(&config, startup_variant);
        let extension_root = config
            .config_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("extensions");
        let state = AppState::new_for_window(
            config,
            window_state_key,
            backends,
            repaint,
            Some(direct_input_rx),
            Some(modifier_side_rx),
        )?;
        let extensions = ExtensionHost::load_with_ui(
            &extension_root,
            state.command_catalog().extensions_arc(),
            state.app_command_sender(Caller::Luau),
            control_plane.extension_event_sender(),
            extension_theme.clone(),
            bootty_config::config::default_working_directory(),
        );
        let settings = SettingsSurface::new(state.config().clone(), state.config_document());
        Ok(Self {
            state,
            terminal_view,
            chrome: ChromeRuntime::default(),
            settings,
            error_details_open: false,
            error_toast_identity: None,
            error_toast_message: None,
            error_toast_started: None,
            _menu: crate::menu::install(),
            extensions,
            extension_theme,
            window_focused: true,
            extension_windows: crate::ui::extension_window::ExtensionWindows::default(),
        })
    }

    pub(crate) fn control_binding(
        &self,
    ) -> (
        BoundAppCommandSender,
        std::sync::Arc<bootty_control::ControlCatalog>,
    ) {
        let catalog = self.state.command_catalog();
        (
            self.state.app_command_sender(Caller::Socket),
            catalog.control_catalog(),
        )
    }

    fn sync_extension_theme(&mut self, ctx: &egui::Context) {
        if self.state.theme_picker_preview_active() {
            return;
        }
        let next = theme_tokens(self.state.config(), self.state.active_appearance_variant());
        if self.extension_theme == next {
            return;
        }
        self.extension_theme = next.clone();
        self.extensions.update_theme(next);
        ctx.request_repaint();
    }

    fn open_settings(&mut self, ctx: &egui::Context) {
        if !self.state.settings_open() {
            self.settings
                .reset_accepted_config(self.state.config().clone(), self.state.config_document());
            // Scanning the font database and the themes directory happens here, once per open,
            // never from a page's paint.
            self.settings.set_catalogs(
                bootty_render::font_database::installed_family_names(),
                bootty_config::config::available_theme_names(&self.state.config().config_path),
            );
        }
        self.state.set_settings_open(true);
        ctx.request_repaint();
    }

    /// Keep the settings schema and the modules' accepted values in step with the extension host.
    /// Cheap per frame: the schema rebuilds only when the declaration set changes, and publishing
    /// values is a no-op when they match.
    fn sync_extension_settings(&mut self) {
        let (declarations, revision) = self.extensions.setting_declarations();
        let declarations: Vec<bootty_config::settings_schema::ExtensionSetting> = declarations
            .iter()
            .map(
                |declaration| bootty_config::settings_schema::ExtensionSetting {
                    module: declaration.module.clone(),
                    key: declaration.key.clone(),
                    label: declaration.label.clone(),
                    help: declaration.help.clone(),
                    default: declaration.default.clone(),
                },
            )
            .collect();
        self.state.sync_settings_schema(&declarations, revision);
        self.extensions
            .update_settings(self.state.extension_settings());
    }

    fn show_settings(&mut self, ui: &mut egui::Ui) {
        if !self.state.settings_open() {
            // Covers closes that bypass the Close return below (e.g. a toggle
            // keybind); idempotent once the style is already restored.
            self.settings.restore_global_style(ui.ctx());
            return;
        }
        self.settings.set_schema(self.state.settings_schema());
        let revision = self.state.config_revision();
        if self.settings.needs_accepted_config(revision) {
            self.settings.sync_accepted_config(
                self.state.config().clone(),
                self.state.config_document(),
                revision,
            );
        }
        let theme = self.state.ui_theme();
        let captured_chords = self.state.take_settings_capture_chords();
        let modifier_sides = self.state.modifier_sides();
        let action = self.settings.show(
            ui,
            theme,
            captured_chords,
            modifier_sides,
            self.extensions.module_sources(),
        );
        for request in self.settings.take_module_requests() {
            let outcome = self.extensions.apply_module_source_request(request);
            self.settings.apply_module_outcome(outcome);
            ui.ctx().request_repaint();
        }
        if let Some((profile, sender)) = self.settings.take_remote_test() {
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                let result = crate::remote_catalog::list_remote(&profile)
                    .map(|_| ())
                    .map_err(|error| error.to_string());
                let _ = sender.send(result);
                ctx.request_repaint();
            });
        }
        let accepted = if let Some(document) = self.settings.take_document_submission() {
            match self.state.commit_settings_document(document) {
                Ok((document, warning, effects)) => {
                    self.apply_effects(ui.ctx(), effects);
                    self.settings.rebind_accepted_config(
                        self.state.config().clone(),
                        document,
                        warning,
                    );
                    true
                }
                Err(error) => {
                    self.settings.reject_submission(&error);
                    false
                }
            }
        } else {
            true
        };
        if action == SettingsAction::Close && accepted {
            self.state.set_settings_open(false);
            self.settings.restore_global_style(ui.ctx());
            ui.ctx().request_repaint();
        }
    }

    fn apply_effects(&mut self, ctx: &egui::Context, effects: Vec<AppEffect>) {
        for effect in effects {
            match effect {
                AppEffect::CloseWindow => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                AppEffect::QuitApplication => {
                    let viewport_ids =
                        ctx.input(|input| input.raw.viewports.keys().copied().collect::<Vec<_>>());
                    for viewport_id in viewport_ids {
                        ctx.send_viewport_cmd_to(viewport_id, egui::ViewportCommand::Close);
                    }
                }
                AppEffect::SetWindowTitle(title) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title(title))
                }
                AppEffect::SetFullscreen(fullscreen) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(fullscreen))
                }
                AppEffect::SetMaximized(maximized) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(maximized))
                }
                AppEffect::SetDecorations(decorations) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(decorations))
                }
                AppEffect::RequestCopy => ctx.send_viewport_cmd(egui::ViewportCommand::RequestCopy),
                AppEffect::RequestRepaint => ctx.request_repaint(),
                AppEffect::Bell => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                        egui::UserAttentionType::Informational,
                    ))
                }
                AppEffect::RepaintAfter(after) => ctx.request_repaint_after(after),
                AppEffect::SetTerminalTextConfig(text_config) => {
                    self.terminal_view.set_text_config(text_config);
                }
                AppEffect::SetTerminalCursorIcon(icon) => {
                    self.terminal_view.set_cursor_icon(icon);
                    ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(
                        icon != egui::CursorIcon::None,
                    ));
                }
                AppEffect::SetUiFonts(families) => configure_egui_fonts(ctx, &families),
                AppEffect::SetWindowFocus => ctx.send_viewport_cmd(egui::ViewportCommand::Focus),
                AppEffect::OpenUrl(url) => ctx.open_url(egui::OpenUrl::new_tab(url)),
                AppEffect::OpenSettings => self.open_settings(ctx),
                AppEffect::ConfigureKeybind(action) => {
                    self.open_settings(ctx);
                    self.settings.focus_keybinding(&action);
                }
            }
        }
    }

    fn show_modal_dialog(&mut self, ctx: &egui::Context) {
        let theme = self.state.ui_theme();
        // Two dialogs need a read of the workspace before the modal is borrowed mutably; take
        // those first, only when that dialog is actually open.
        let open_cwds = matches!(self.state.modal_dialog(), Some(ModalDialog::NewSession(_)))
            .then(|| {
                self.state
                    .mux()
                    .sessions()
                    .iter()
                    .filter_map(|session| session.anchor.cwd.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let groups = matches!(
            self.state.modal_dialog(),
            Some(ModalDialog::SessionPicker(_))
        )
        .then(|| self.state.session_finder_groups())
        .unwrap_or_default();

        // What each dialog returned, applied after the borrow ends.
        enum Outcome {
            None,
            Picker(crate::ui::new_session_picker::NewSessionPickerEvent),
            SpaceEditor(crate::ui::space::SpaceEditorIntent),
            SessionPicker(crate::ui::session_picker::SessionPickerEvent),
            RenameSession(crate::ui::rename::RenameSessionEvent),
            RenameTab(crate::ui::rename::RenameTabEvent),
            Ditch(crate::ui::ditch::DitchSessionEvent),
            DismissKeybindHelp,
            CommandPalette(crate::ui::command_palette::CommandPaletteEvent),
            ThemePicker(crate::ui::theme_picker::ThemePickerEvent),
            SpacePicker(crate::ui::space_picker::SpacePickerEvent),
        }

        let outcome = match self.state.modal_dialog_mut() {
            None => return,
            Some(ModalDialog::NewSession(dialog)) => dialog
                .show(ctx, theme, &open_cwds)
                .map_or(Outcome::None, Outcome::Picker),
            Some(ModalDialog::SpaceEditor(dialog)) => dialog
                .show(ctx, theme)
                .map_or(Outcome::None, Outcome::SpaceEditor),
            Some(ModalDialog::SessionPicker(dialog)) => dialog
                .show(ctx, theme, &groups)
                .map_or(Outcome::None, Outcome::SessionPicker),
            Some(ModalDialog::RenameSession(dialog)) => dialog
                .show(ctx, theme)
                .map_or(Outcome::None, Outcome::RenameSession),
            Some(ModalDialog::RenameTab(dialog)) => dialog
                .show(ctx, theme)
                .map_or(Outcome::None, Outcome::RenameTab),
            Some(ModalDialog::DitchSession(dialog)) => dialog
                .show(ctx, theme)
                .map_or(Outcome::None, Outcome::Ditch),
            Some(ModalDialog::KeybindHelp(dialog)) => {
                if dialog.show(ctx, theme) {
                    Outcome::DismissKeybindHelp
                } else {
                    Outcome::None
                }
            }
            Some(ModalDialog::CommandPalette(dialog)) => dialog
                .show(ctx, theme)
                .map_or(Outcome::None, Outcome::CommandPalette),
            Some(ModalDialog::ThemePicker(dialog)) => dialog
                .show(ctx, theme)
                .map_or(Outcome::None, Outcome::ThemePicker),
            Some(ModalDialog::SpacePicker(dialog)) => dialog
                .show(ctx, theme)
                .map_or(Outcome::None, Outcome::SpacePicker),
        };

        match outcome {
            Outcome::None => {}
            Outcome::Picker(event) => self.state.apply_picker_event(event),
            Outcome::SpaceEditor(intent) => self.state.apply_space_editor_intent(intent),
            Outcome::SessionPicker(event) => self.state.apply_session_picker_event(event),
            Outcome::RenameSession(event) => self.state.apply_rename_session_event(event),
            Outcome::RenameTab(event) => self.state.apply_rename_tab_event(event),
            Outcome::Ditch(event) => self.state.apply_ditch_session_event(event),
            Outcome::DismissKeybindHelp => self.state.dismiss_keybind_help(),
            Outcome::SpacePicker(event) => self.state.apply_space_picker_event(event),
            Outcome::CommandPalette(event) => {
                let run = matches!(
                    event,
                    crate::ui::command_palette::CommandPaletteEvent::Run(_)
                );
                self.state.apply_command_palette_event(event);
                if run {
                    ctx.request_repaint();
                }
            }
            Outcome::ThemePicker(event) => {
                // Preview, restore and select all run before the effects they produce.
                let mut effects = Vec::new();
                self.state.apply_theme_picker_event(event, &mut effects);
                self.apply_effects(ctx, effects);
            }
        }
    }

    fn show_terminal_find_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.state.take_terminal_find_dialog() else {
            return;
        };
        let event = dialog.show(ctx, self.state.ui_theme());
        let searched = matches!(
            event,
            crate::ui::terminal_find::TerminalFindEvent::Search { .. }
        );
        self.state.apply_terminal_find_event(dialog, event);
        if searched {
            ctx.request_repaint();
        }
    }

    fn show_extension_surfaces(&mut self, ctx: &egui::Context) {
        let floating = self.extensions.surfaces(SurfacePlacement::Floating);
        self.extension_windows
            .retain_open(|id| floating.iter().any(|s| s.snapshot.declaration.id == id));
        let theme = self.state.ui_theme();
        for surface in floating {
            // An empty surface is a module choosing not to show a window this frame.
            if surface.snapshot.items.is_empty() {
                continue;
            }
            let event = self.extension_windows.show(ctx, theme, &surface);
            match event {
                Some(crate::ui::extension_window::ExtensionWindowEvent::Action(action)) => {
                    self.submit_extension_action(surface, action);
                }
                Some(crate::ui::extension_window::ExtensionWindowEvent::Dismissed) => {
                    self.submit_extension_action(surface, DISMISS_ACTION.to_owned());
                }
                None => {}
            }
        }
        for surface in self.extensions.surfaces(SurfacePlacement::Docked) {
            let mut action = None;
            egui::Area::new(egui::Id::new((
                "extension-docked",
                surface.module.clone(),
                surface.snapshot.declaration.id.clone(),
            )))
            .anchor(egui::Align2::RIGHT_CENTER, egui::vec2(-12.0, 0.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(240.0);
                    ui.heading(&surface.snapshot.declaration.id);
                    action = show_extension_surface_items(ui, &surface.snapshot.items);
                });
            });
            if let Some(action) = action {
                self.submit_extension_action(surface, action);
            }
        }
    }

    fn submit_extension_action(&mut self, surface: PublishedSurfaceSnapshot, action: String) {
        let _ = self.extensions.submit_ui_action(ExtensionUiAction {
            module: surface.module,
            generation: surface.generation,
            surface: surface.snapshot.declaration.id,
            action,
            payload: serde_json::Value::Null,
        });
    }
}

/// The action a floating surface receives when the user dismisses it, so the module can stop
/// publishing items instead of the window reappearing next frame.
const DISMISS_ACTION: &str = "dismiss";

fn show_extension_surface_items(ui: &mut egui::Ui, items: &[ModuleItem]) -> Option<String> {
    let mut selected = None;
    for item in items {
        match item.action.as_ref() {
            Some(action) => {
                if ui.button(&item.text).clicked() {
                    selected = Some(action.clone());
                }
            }
            None => {
                ui.label(&item.text);
            }
        }
    }
    selected
}

impl BoottyApp {
    fn sync_error_toast(&mut self, notice: &ErrorNotice, now: Instant) {
        let identity = notice.raw_message();
        if self.error_toast_identity.as_deref() == Some(identity.as_str()) {
            return;
        }
        self.error_toast_identity = Some(identity);
        self.error_toast_message = Some(notice.to_string());
        self.error_toast_started = Some(now);
        self.error_details_open = false;
    }

    fn reset_error_toast(&mut self) {
        self.error_details_open = false;
        self.error_toast_identity = None;
        self.error_toast_message = None;
        self.error_toast_started = None;
    }

    fn dismiss_error_toast(&mut self) {
        self.state.clear_last_error();
        self.reset_error_toast();
    }
}

impl eframe::App for BoottyApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.state.drain_direct_input();
        self.state
            // A declaration is permanent; a *shown* window is not. Gate on items, or any module
            // declaring a floating surface would hold the terminal's keyboard for the session.
            .set_extension_overlay_open(
                self.extensions
                    .surfaces(SurfacePlacement::Floating)
                    .iter()
                    .any(|surface| !surface.snapshot.items.is_empty()),
            );
        if self.state.settings_open() {
            if self.settings.is_recording_keybind() {
                suppress_egui_events_for_direct_input(
                    &mut raw_input.events,
                    self.state.pending_direct_input(),
                );
            }
            return;
        }
        if self.state.direct_input_suppresses_egui_events() {
            suppress_egui_events_for_direct_input(
                &mut raw_input.events,
                self.state.pending_direct_input(),
            );
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.window_focused = ctx.input(|input| input.viewport().focused.unwrap_or(true));
        let (
            mut events,
            mut dropped_file_paths,
            modifiers,
            hover_pos,
            pressed_mouse_button,
            viewport,
            zoom_delta,
        ) = ctx.input(|input| {
            (
                input.events.clone(),
                input
                    .raw
                    .dropped_files
                    .iter()
                    .map(|file| file.path().to_path_buf())
                    .collect::<Vec<PathBuf>>(),
                input.modifiers,
                input.pointer.hover_pos(),
                crate::input::pressed_mouse_button_from_egui(&input.pointer),
                ViewportSnapshot {
                    fullscreen: input.viewport().fullscreen.unwrap_or(false),
                    maximized: input.viewport().maximized.unwrap_or(false),
                    content_height: input.content_rect().height(),
                },
                input.zoom_delta(),
            )
        });
        let settings_open = self.state.settings_open();
        if settings_open {
            events.clear();
            dropped_file_paths.clear();
        }

        let terminal_view =
            self.terminal_view
                .update_input(!settings_open, zoom_delta, hover_pos, &mut events);

        let now = Instant::now();
        self.extensions.refresh(now);
        self.sync_extension_settings();
        let inputs = FrameInputs {
            now,
            events,
            dropped_file_paths,
            modifiers,
            hover_pos,
            pressed_mouse_button,
            viewport,
            window_focused: self.window_focused,
            renderer_metrics: terminal_view.renderer_metrics,
            terminal_cell_width: terminal_view.cell_width,
            terminal_cell_height: terminal_view.cell_height,
            terminal_scale_factor: ctx.pixels_per_point(),
            terminal_view_transform: terminal_view.view_transform,
        };
        let effects = self.state.update_frame(inputs);
        self.apply_effects(ctx, effects);
        if animate_indeterminate_progress(
            self.window_focused,
            self.state.has_indeterminate_terminal_progress(),
        ) {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }

        if crate::menu::settings_requested() {
            self.open_settings(ctx);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let system_variant = match ui.ctx().system_theme().unwrap_or(egui::Theme::Dark) {
            egui::Theme::Light => AppearanceVariant::Light,
            egui::Theme::Dark => AppearanceVariant::Dark,
        };
        let variant = self.state.config().appearance.mode.variant(system_variant);
        self.state.set_appearance_variant(variant);
        self.sync_extension_theme(ui.ctx());
        let theme = self.state.ui_theme();
        let mut style = (*ui.ctx().global_style()).clone();
        bootty_ui::configure_style(&mut style, theme);
        ui.ctx().set_global_style(style);
        let palette = theme.palette;
        egui::Frame::NONE.fill(palette.mantle).show(ui, |ui| {
            if self.state.settings_open() {
                self.show_settings(ui);
            } else {
                // Apply session-order changes any extension module requested via
                // `bootty.reorder_session` before publishing the snapshot, so the reordered
                // sessions render on the next tick.
                for reorder in self.extensions.take_session_reorders() {
                    self.state
                        .reorder_session_before(&reorder.source, reorder.before.as_deref());
                }
                let projection = crate::chrome_frame::prepare(
                    &self.state,
                    self.state.config().chrome.sidebar,
                    self.window_focused,
                );
                self.extensions.update_mux(projection.mux);
                let frame = self.chrome.show(
                    ui,
                    &self.state,
                    &self.extensions,
                    projection.tab_context.as_ref(),
                    self.terminal_view.cell_height(),
                );
                // Chrome interactions land before the terminal is painted, so a session or Space
                // switch shows its own terminal in the same frame.
                let effects = crate::chrome_frame::apply(
                    ui.ctx(),
                    &mut self.state,
                    &mut self.extensions,
                    frame.events,
                );
                self.apply_effects(ui.ctx(), effects);
                self.state.set_chrome_handles(frame.handles);
                let terminal = frame.terminal;
                self.terminal_view.show(
                    &mut self.state,
                    ui,
                    terminal.rect,
                    terminal.palette,
                    terminal.pane_backing_color,
                    terminal.notch_chrome_color,
                );
            }
        });
        let now = Instant::now();
        let mut dismiss_error = false;
        if let Some(notice) = self.state.error_notice() {
            self.sync_error_toast(&notice, now);
            let expired = !self.error_details_open
                && self.error_toast_started.is_some_and(|started| {
                    now.duration_since(started) >= error_catalog::auto_dismiss_after()
                });
            if expired {
                dismiss_error = true;
            } else {
                let remaining = self.error_toast_started.and_then(|started| {
                    error_catalog::auto_dismiss_after().checked_sub(now.duration_since(started))
                });
                if !self.error_details_open
                    && let Some(remaining) = remaining
                {
                    ui.ctx().request_repaint_after(remaining);
                }

                let mut close = false;
                egui::Area::new(egui::Id::new("last-error"))
                    .order(egui::Order::Foreground)
                    .anchor(egui::Align2::RIGHT_TOP, [-16.0, 16.0])
                    .show(ui.ctx(), |ui| {
                        let max_width =
                            (ui.ctx().content_rect().width() - 32.0).clamp(280.0, 760.0);
                        let width = if self.error_details_open {
                            max_width.min(640.0)
                        } else {
                            max_width.min(400.0)
                        };
                        egui::Frame::NONE
                            .fill(palette.pane)
                            .stroke(egui::Stroke::new(
                                1.0,
                                palette.destructive.gamma_multiply(0.65),
                            ))
                            .corner_radius(egui::CornerRadius::same(palette.radius))
                            .inner_margin(egui::Margin::symmetric(14, 12))
                            .show(ui, |ui| {
                                ui.set_width(width);
                                let content_width = (ui.available_width() - 32.0).max(0.0);
                                ui.horizontal_top(|ui| {
                                    ui.vertical(|ui| {
                                        ui.set_width(content_width);
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(notice.to_string())
                                                    .color(palette.destructive)
                                                    .strong(),
                                            )
                                            .wrap(),
                                        );
                                        if let Some(details) = notice.details() {
                                            let response = egui::CollapsingHeader::new(
                                                egui::RichText::new("Technical details")
                                                    .color(palette.subtext)
                                                    .size(12.0),
                                            )
                                            .id_salt("last-error-details")
                                            .open(Some(self.error_details_open))
                                            .icon(|ui, openness, response| {
                                                let center = response.rect.center();
                                                let closed = [
                                                    egui::pos2(center.x - 3.0, center.y - 4.0),
                                                    egui::pos2(center.x + 3.0, center.y),
                                                    egui::pos2(center.x - 3.0, center.y + 4.0),
                                                ];
                                                let open = [
                                                    egui::pos2(center.x - 4.0, center.y - 2.0),
                                                    egui::pos2(center.x, center.y + 3.0),
                                                    egui::pos2(center.x + 4.0, center.y - 2.0),
                                                ];
                                                let interpolate =
                                                    |from: egui::Pos2, to: egui::Pos2| {
                                                        from + (to - from) * openness
                                                    };
                                                let stroke =
                                                    ui.style().interact(response).fg_stroke;
                                                ui.painter().line_segment(
                                                    [
                                                        interpolate(closed[0], open[0]),
                                                        interpolate(closed[1], open[1]),
                                                    ],
                                                    stroke,
                                                );
                                                ui.painter().line_segment(
                                                    [
                                                        interpolate(closed[1], open[1]),
                                                        interpolate(closed[2], open[2]),
                                                    ],
                                                    stroke,
                                                );
                                            })
                                            .show_unindented(ui, |ui| {
                                                egui::Frame::NONE
                                                    .fill(palette.base)
                                                    .stroke(egui::Stroke::new(1.0, palette.border))
                                                    .corner_radius(egui::CornerRadius::same(
                                                        palette.radius,
                                                    ))
                                                    .inner_margin(egui::Margin::symmetric(10, 8))
                                                    .show(ui, |ui| {
                                                        egui::ScrollArea::vertical()
                                                            .max_height(360.0)
                                                            .show(ui, |ui| {
                                                                ui.add(
                                                                    egui::Label::new(
                                                                        egui::RichText::new(
                                                                            details,
                                                                        )
                                                                        .monospace()
                                                                        .size(11.0)
                                                                        .color(palette.subtext),
                                                                    )
                                                                    .wrap(),
                                                                );
                                                            });
                                                    });
                                            });
                                            if response.header_response.clicked() {
                                                self.error_details_open = !self.error_details_open;
                                                self.error_toast_started =
                                                    if self.error_details_open {
                                                        None
                                                    } else {
                                                        Some(now)
                                                    };
                                                ui.ctx().request_repaint();
                                            }
                                        }
                                    });
                                    ui.add_space(8.0);
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                egui::RichText::new("×")
                                                    .size(18.0)
                                                    .color(palette.muted),
                                            )
                                            .frame(false)
                                            .min_size(egui::vec2(24.0, 24.0)),
                                        )
                                        .on_hover_text("Dismiss")
                                        .clicked()
                                    {
                                        close = true;
                                    }
                                });
                            });
                    });
                if close {
                    dismiss_error = true;
                }
            }
        } else if self.error_toast_message.is_some() {
            self.reset_error_toast();
        }
        if dismiss_error {
            self.dismiss_error_toast();
        }
        if !self.state.settings_open() {
            self.show_modal_dialog(ui.ctx());
            self.show_terminal_find_dialog(ui.ctx());
            self.show_extension_surfaces(ui.ctx());
        }
        let cursor_icon = ui.ctx().output(|output| output.cursor_icon);
        bootty_winit::window::set_macos_cursor_icon(cursor_icon);
    }
}

fn configure_egui_fonts(ctx: &egui::Context, families: &[String]) {
    let mut fonts = bootty_render::font_database::ui_font_definitions(families);
    bootty_ui::icons::add_icon_fonts(&mut fonts);
    ctx.set_fonts(fonts);
}
