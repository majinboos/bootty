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

use crate::renderer::{TerminalWorkspaceView, animate_indeterminate_progress};
use crate::state::{AppEffect, AppState, FrameInputs, ViewportSnapshot};
use crate::ui::chrome::ChromeRuntime;

use crate::{
    menu::AppMenu,
    theme::theme_tokens,
    ui::{
        ModalDialog, ModalKind,
        settings::{SettingsAction, SettingsSurface},
    },
};

pub struct BoottyApp {
    state: AppState,
    terminal_view: TerminalWorkspaceView,
    chrome: ChromeRuntime,
    settings: SettingsSurface,
    error_details_open: bool,
    // Held for the process lifetime so the native menu stays installed.
    _menu: Option<AppMenu>,
    extensions: ExtensionHost,
    extension_theme: Vec<(String, String)>,
    /// Whether the window had keyboard focus this frame. Extension hosts throttle themselves while
    /// it is false, so an unfocused window stops animating (and repainting) its chrome.
    window_focused: bool,
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
            _menu: crate::menu::install(),
            extensions,
            extension_theme,
            window_focused: true,
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

    fn show_settings(&mut self, ui: &mut egui::Ui) {
        if !self.state.settings_open() {
            // Covers closes that bypass the Close return below (e.g. a toggle
            // keybind); idempotent once the style is already restored.
            self.settings.restore_global_style(ui.ctx());
            return;
        }
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
        let Some(kind) = self.state.modal_dialog().map(ModalDialog::kind) else {
            return;
        };
        let theme = self.state.ui_theme();
        match kind {
            ModalKind::NewSession => {
                let open_cwds = self
                    .state
                    .mux()
                    .sessions()
                    .iter()
                    .filter_map(|session| session.anchor.cwd.clone())
                    .collect::<Vec<_>>();
                let Some(ModalDialog::NewSession(dialog)) = self.state.modal_dialog_mut() else {
                    return;
                };
                let event = dialog.show(ctx, theme, &open_cwds);
                if let Some(event) = event {
                    self.state.apply_picker_event(event);
                }
            }
            ModalKind::SpaceEditor => {
                let Some(ModalDialog::SpaceEditor(dialog)) = self.state.modal_dialog_mut() else {
                    return;
                };
                let intent = dialog.show(ctx, theme);
                if let Some(intent) = intent {
                    self.state.apply_space_editor_intent(intent);
                }
            }
            ModalKind::SessionPicker => {
                let groups = self.state.session_finder_groups();
                let Some(ModalDialog::SessionPicker(dialog)) = self.state.modal_dialog_mut() else {
                    return;
                };
                let event = dialog.show(ctx, theme, &groups);
                if let Some(event) = event {
                    self.state.apply_session_picker_event(event);
                }
            }
            ModalKind::RenameSession => {
                let Some(ModalDialog::RenameSession(dialog)) = self.state.modal_dialog_mut() else {
                    return;
                };
                let event = dialog.show(ctx, theme);
                if let Some(event) = event {
                    self.state.apply_rename_session_event(event);
                }
            }
            ModalKind::RenameTab => {
                let Some(ModalDialog::RenameTab(dialog)) = self.state.modal_dialog_mut() else {
                    return;
                };
                let event = dialog.show(ctx, theme);
                if let Some(event) = event {
                    self.state.apply_rename_tab_event(event);
                }
            }
            ModalKind::DitchSession => {
                let Some(ModalDialog::DitchSession(dialog)) = self.state.modal_dialog_mut() else {
                    return;
                };
                let event = dialog.show(ctx, theme);
                if let Some(event) = event {
                    self.state.apply_ditch_session_event(event);
                }
            }
            ModalKind::KeybindHelp => {
                let Some(ModalDialog::KeybindHelp(dialog)) = self.state.modal_dialog_mut() else {
                    return;
                };
                if dialog.show(ctx, theme) {
                    self.state.dismiss_keybind_help();
                }
            }
            ModalKind::CommandPalette => {
                let Some(ModalDialog::CommandPalette(dialog)) = self.state.modal_dialog_mut()
                else {
                    return;
                };
                let event = dialog.show(ctx, theme);
                if let Some(event) = event {
                    let run = matches!(
                        event,
                        crate::ui::command_palette::CommandPaletteEvent::Run(_)
                    );
                    self.state.apply_command_palette_event(event);
                    if run {
                        ctx.request_repaint();
                    }
                }
            }
            ModalKind::ThemePicker => {
                let Some(ModalDialog::ThemePicker(dialog)) = self.state.modal_dialog_mut() else {
                    return;
                };
                let event = dialog.show(ctx, theme);
                if let Some(event) = event {
                    let mut effects = Vec::new();
                    self.state.apply_theme_picker_event(event, &mut effects);
                    self.apply_effects(ctx, effects);
                }
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
        for surface in self.extensions.surfaces(SurfacePlacement::Floating) {
            let mut action = None;
            egui::Window::new(surface.snapshot.declaration.id.clone())
                .id(egui::Id::new((
                    "extension-floating",
                    surface.module.clone(),
                    surface.snapshot.declaration.id.clone(),
                )))
                .collapsible(false)
                .show(ctx, |ui| {
                    action = show_extension_surface_items(ui, &surface.snapshot.items);
                });
            if let Some(action) = action {
                self.submit_extension_action(surface, action);
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct ErrorToastText {
    summary: String,
    details: Option<String>,
}

fn error_toast_text(error: &str) -> ErrorToastText {
    let normalized = error.trim();
    let lower = normalized.to_ascii_lowercase();
    let summary = if lower.contains("rmux") {
        "Could not reach remote rmux.".to_owned()
    } else if lower.contains("ssh") || lower.contains("connection") {
        "Could not reach the remote workspace.".to_owned()
    } else {
        let first_line = normalized.lines().next().unwrap_or("Operation failed.");
        if first_line.chars().count() <= 96 {
            first_line.to_owned()
        } else {
            "The operation failed. Open details for the technical error.".to_owned()
        }
    };
    let details = (summary != normalized).then(|| normalized.to_owned());
    ErrorToastText { summary, details }
}

impl eframe::App for BoottyApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.state.drain_direct_input();
        self.state
            .set_extension_overlay_open(self.extensions.has_surfaces(SurfacePlacement::Floating));
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
        if let Some(error) = self.state.last_error().map(str::to_owned) {
            let toast = error_toast_text(&error);
            let mut dismiss = false;
            egui::Area::new(egui::Id::new("last-error"))
                .order(egui::Order::Tooltip)
                .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -12.0])
                .show(ui.ctx(), |ui| {
                    let max_width = (ui.ctx().content_rect().width() - 48.0).clamp(280.0, 560.0);
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width(max_width);
                        ui.vertical(|ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&toast.summary).color(palette.destructive),
                                )
                                .wrap(),
                            );
                            ui.horizontal(|ui| {
                                if toast.details.is_some()
                                    && ui
                                        .button(if self.error_details_open {
                                            "Hide details"
                                        } else {
                                            "Details"
                                        })
                                        .clicked()
                                {
                                    self.error_details_open = !self.error_details_open;
                                }
                                dismiss = ui.button("Dismiss").clicked();
                            });
                            if self.error_details_open
                                && let Some(details) = &toast.details
                            {
                                egui::ScrollArea::vertical()
                                    .max_height(180.0)
                                    .show(ui, |ui| {
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(details)
                                                    .monospace()
                                                    .size(11.0)
                                                    .color(palette.subtext),
                                            )
                                            .wrap(),
                                        );
                                    });
                            }
                        });
                    });
                });
            if dismiss {
                self.error_details_open = false;
                self.state.clear_last_error();
            }
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
