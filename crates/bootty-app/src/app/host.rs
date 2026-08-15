use super::terminal_config::terminal_text_config;

use std::{path::PathBuf, sync::mpsc, time::Instant};

use anyhow::Result;
use eframe::egui::{self, FontData, FontDefinitions, FontFamily};

use super::chrome_runtime::ChromeRuntime;
use super::state::{AppEffect, AppState, FrameInputs, ViewportSnapshot};
use super::terminal_workspace_view::{TerminalWorkspaceView, animate_indeterminate_progress};

use crate::{
    commands::{BoundAppCommandSender, Caller, CommandCatalog},
    config::{AppearanceVariant, BoottyConfig},
    control::ControlPlane,
    direct_input::{DirectKeyInput, ModifierSideState, suppress_egui_events_for_direct_input},
    menu::AppMenu,
    theme::theme_tokens,
    ui::settings::{SettingsAction, SettingsSurface},
};

const EGUI_SYMBOL_FALLBACK_FAMILIES: &[&str] = &[
    "Apple Symbols",
    "Segoe UI Symbol",
    "Noto Sans Symbols 2",
    "Noto Sans Symbols",
    "DejaVu Sans",
    "Symbola",
    "Arial Unicode MS",
];
const EGUI_SYMBOL_FALLBACK_CHARS: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

fn sync_global_egui_style(ctx: &egui::Context, theme: bootty_ui::Theme) {
    let mut style = (*ctx.global_style()).clone();
    bootty_ui::configure_style(&mut style, theme);
    ctx.set_global_style(style);
}

pub struct BoottyApp {
    state: AppState,
    terminal_view: TerminalWorkspaceView,
    chrome: ChromeRuntime,
    settings_open: bool,
    settings: SettingsSurface,
    error_details_open: bool,
    // Held for the process lifetime so the native menu stays installed.
    _menu: Option<AppMenu>,
    extensions: crate::command_extensions::ExtensionHost,
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
        backends: std::sync::Arc<bootty_mux::provider::MuxAppBackendRegistry>,
        direct_input_rx: mpsc::Receiver<DirectKeyInput>,
        modifier_side_rx: mpsc::Receiver<ModifierSideState>,
        control_plane: ControlPlane,
    ) -> Result<Self> {
        let direct_input_rx = Some(direct_input_rx);
        let modifier_side_rx = Some(modifier_side_rx);
        if uses_custom_egui_fonts(&config) {
            configure_egui_fonts(&cc.egui_ctx, config.font.ui_families());
        } else {
            crate::ui::icons::install_icon_fonts(&cc.egui_ctx);
        }
        let repaint_ctx = cc.egui_ctx.clone();
        let repaint: crate::mux::RepaintHandle =
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
        let config_dir = config
            .config_path
            .parent()
            .unwrap_or(std::path::Path::new("."));
        let state = AppState::new_for_window(
            config.clone(),
            window_state_key,
            backends,
            repaint,
            direct_input_rx,
            modifier_side_rx,
        )?;
        let extensions = crate::command_extensions::ExtensionHost::load_with_ui(
            &config_dir.join("extensions"),
            state.command_catalog(),
            state.app_command_sender(Caller::Luau),
            control_plane.clone(),
            extension_theme.clone(),
        );
        Ok(Self {
            state,
            terminal_view,
            chrome: ChromeRuntime::default(),
            settings_open: false,
            settings: SettingsSurface::new(config.clone()),
            error_details_open: false,
            _menu: crate::menu::install(),
            extensions,
            extension_theme,
            window_focused: true,
        })
    }

    pub(crate) fn control_binding(
        &self,
    ) -> (BoundAppCommandSender, std::sync::Arc<CommandCatalog>) {
        (
            self.state.app_command_sender(Caller::Socket),
            self.state.command_catalog(),
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
        self.settings_open = true;
        self.state.set_settings_open(true);
        ctx.request_repaint();
    }

    fn show_settings(&mut self, ui: &mut egui::Ui) {
        if !self.settings_open {
            // Covers closes that bypass the Close return below (e.g. a toggle
            // keybind); idempotent once the style is already restored.
            self.settings.restore_global_style(ui.ctx());
            return;
        }
        let theme = self.state.ui_theme();
        let captured_chords = self.state.take_settings_capture_chords();
        let modifier_sides = self.state.modifier_sides();
        if self
            .settings
            .show(ui, theme, captured_chords, modifier_sides)
            == SettingsAction::Close
        {
            self.settings_open = false;
            self.state.set_settings_open(false);
            self.settings.restore_global_style(ui.ctx());
            ui.ctx().request_repaint();
        }
    }

    fn apply_effects(&mut self, ctx: &egui::Context, effects: Vec<AppEffect>) {
        for effect in effects {
            match effect {
                AppEffect::CloseWindow => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                AppEffect::QuitApplication => {
                    let viewport_ids =
                        ctx.input(|input| input.raw.viewports.keys().copied().collect::<Vec<_>>());
                    for viewport_id in viewport_ids {
                        ctx.send_viewport_cmd_to(viewport_id, egui::ViewportCommand::Close);
                    }
                }
                AppEffect::SetWindowTitle(title) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
                }
                AppEffect::SetFullscreen(fullscreen) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(fullscreen));
                }
                AppEffect::SetMaximized(maximized) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(maximized));
                }
                AppEffect::SetDecorations(decorations) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(decorations));
                }
                AppEffect::RequestCopy => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::RequestCopy);
                }
                AppEffect::RequestRepaint => ctx.request_repaint(),
                AppEffect::Bell => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                        egui::UserAttentionType::Informational,
                    ));
                }
                AppEffect::RepaintAfter(after) => ctx.request_repaint_after(after),
                AppEffect::SetTerminalTextConfig(text_config) => {
                    self.terminal_view.set_text_config(text_config);
                }
                AppEffect::SetTerminalCursorIcon(icon) => {
                    self.terminal_view.set_cursor_icon(icon);
                }
                AppEffect::SetUiFonts(families) => {
                    configure_egui_fonts(ctx, &families);
                }
                AppEffect::SetWindowFocus => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                AppEffect::OpenUrl(url) => {
                    ctx.open_url(egui::OpenUrl::new_tab(url));
                }
                AppEffect::OpenSettings => self.open_settings(ctx),
                AppEffect::ConfigureKeybind(action) => {
                    self.open_settings(ctx);
                    self.settings.focus_keybinding(&action);
                }
            }
        }
    }

    fn show_modal_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.state.take_modal_dialog() else {
            return;
        };
        match dialog {
            super::ModalDialog::NewSession(mut dialog) => {
                let open_cwds = self
                    .state
                    .mux()
                    .sessions()
                    .iter()
                    .filter_map(|session| session.anchor.cwd.clone())
                    .collect::<Vec<_>>();
                let event = dialog.show(ctx, self.state.ui_theme(), &open_cwds);
                self.state.apply_picker_event(dialog, event);
            }
            super::ModalDialog::SpaceEditor(mut dialog) => {
                let event = dialog.show(ctx, self.state.ui_theme());
                self.state.apply_space_editor_event(dialog, event);
            }
            super::ModalDialog::SessionPicker(mut dialog) => {
                let groups = self.state.session_finder_groups();
                let event = dialog.show(ctx, self.state.ui_theme(), &groups);
                self.state.apply_session_picker_event(dialog, event);
            }
            super::ModalDialog::RenameSession(mut dialog) => {
                let event = dialog.show(ctx, self.state.ui_theme());
                self.state.apply_rename_session_event(dialog, event);
            }
            super::ModalDialog::RenameTab(mut dialog) => {
                let event = dialog.show(ctx, self.state.ui_theme());
                self.state.apply_rename_tab_event(dialog, event);
            }
            super::ModalDialog::DitchSession(mut dialog) => {
                let event = dialog.show(ctx, self.state.ui_theme());
                self.state.apply_ditch_session_event(dialog, event);
            }
            super::ModalDialog::KeybindHelp(mut dialog) => {
                let event = dialog.show(ctx, self.state.ui_theme());
                self.state.apply_keybind_help_event(dialog, event);
            }
            super::ModalDialog::CommandPalette(mut dialog) => {
                let event = dialog.show(ctx, self.state.ui_theme());
                let run = matches!(
                    event,
                    crate::ui::command_palette::CommandPaletteEvent::Run(_)
                );
                self.state.apply_command_palette_event(dialog, event);
                if run {
                    ctx.request_repaint();
                }
            }
            super::ModalDialog::ThemePicker(mut dialog) => {
                let event = dialog.show(ctx, self.state.ui_theme());
                let mut effects = Vec::new();
                self.state
                    .apply_theme_picker_event(dialog, event, &mut effects);
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
        use crate::command_extensions::SurfacePlacement;

        let mut actions = Vec::new();
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
                actions.push((surface, action));
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
                actions.push((surface, action));
            }
        }
        for (surface, action) in actions {
            let _ =
                self.extensions
                    .submit_ui_action(crate::command_extensions::ExtensionUiAction {
                        module: surface.module,
                        generation: surface.generation,
                        surface: surface.snapshot.declaration.id,
                        action,
                        payload: serde_json::Value::Null,
                    });
        }
    }
}

fn show_extension_surface_items(
    ui: &mut egui::Ui,
    items: &[crate::extension_ui::ModuleItem],
) -> Option<String> {
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

fn suppress_settings_recorder_duplicates(
    events: &mut Vec<egui::Event>,
    direct_inputs: &[crate::direct_input::DirectKeyInput],
    recording: bool,
) {
    if recording {
        suppress_egui_events_for_direct_input(events, direct_inputs);
    }
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
        self.state.set_extension_overlay_open(
            !self
                .extensions
                .surfaces(crate::command_extensions::SurfacePlacement::Floating)
                .is_empty(),
        );
        if self.settings_open {
            suppress_settings_recorder_duplicates(
                &mut raw_input.events,
                self.state.pending_direct_input(),
                self.settings.is_recording_keybind(),
            );
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
        if self.settings_open {
            suppress_terminal_payload_for_settings(&mut events, &mut dropped_file_paths);
        }

        let terminal_view = self.terminal_view.update_input(
            !self.settings_open,
            zoom_delta,
            hover_pos,
            &mut events,
        );

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
        sync_global_egui_style(ui.ctx(), theme);
        let palette = theme.palette;
        egui::Frame::NONE.fill(palette.mantle).show(ui, |ui| {
            if self.settings_open {
                self.show_settings(ui);
            } else {
                self.chrome.show(
                    ui,
                    &mut self.state,
                    &mut self.extensions,
                    &mut self.terminal_view,
                    self.window_focused,
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
        if !self.settings_open {
            self.show_modal_dialog(ui.ctx());
            self.show_terminal_find_dialog(ui.ctx());
            self.show_extension_surfaces(ui.ctx());
        }
        let cursor_icon = ui.ctx().output(|output| output.cursor_icon);
        crate::platform::set_macos_cursor_icon(cursor_icon);
    }
}

fn uses_custom_egui_fonts(config: &BoottyConfig) -> bool {
    config.chrome.sidebar || config.chrome.top_bar || config.chrome.bottom_bar
}

fn suppress_terminal_payload_for_settings(
    events: &mut Vec<egui::Event>,
    dropped_file_paths: &mut Vec<PathBuf>,
) {
    events.clear();
    dropped_file_paths.clear();
}

fn configure_egui_fonts(ctx: &egui::Context, families: &[String]) {
    let db = bootty_render::font_database::system_font_database();
    let mut fonts = FontDefinitions::default();
    let mut loaded_text_font = false;
    for family in families.iter().rev() {
        loaded_text_font |= add_egui_font_family(&mut fonts, db, family, EguiFontPlacement::First);
    }
    if !loaded_text_font {
        add_egui_default_text_font(&mut fonts, db);
    }
    add_egui_symbol_fallback_fonts(&mut fonts, db);
    crate::ui::icons::add_icon_fonts(&mut fonts);
    ctx.set_fonts(fonts);
}

#[derive(Clone, Copy)]
enum EguiFontPlacement {
    First,
    Last,
}

fn add_egui_default_text_font(fonts: &mut FontDefinitions, db: &fontdb::Database) -> bool {
    let query_families = [fontdb::Family::Monospace];
    let query = fontdb::Query {
        families: &query_families,
        ..fontdb::Query::default()
    };
    let Some(id) = db.query(&query) else {
        return false;
    };
    add_egui_font_face(
        fonts,
        db,
        id,
        "bootty-ui-default-monospace",
        EguiFontPlacement::First,
    )
}

fn add_egui_symbol_fallback_fonts(fonts: &mut FontDefinitions, db: &fontdb::Database) {
    for family in EGUI_SYMBOL_FALLBACK_FAMILIES {
        if add_egui_font_family(fonts, db, family, EguiFontPlacement::Last) {
            break;
        }
    }
    for ch in EGUI_SYMBOL_FALLBACK_CHARS {
        add_egui_font_for_char(fonts, db, *ch);
    }
}

fn add_egui_font_family(
    fonts: &mut FontDefinitions,
    db: &fontdb::Database,
    family: &str,
    placement: EguiFontPlacement,
) -> bool {
    let name = egui_font_name(family);
    let query_families = [fontdb::Family::Name(family)];
    let query = fontdb::Query {
        families: &query_families,
        ..fontdb::Query::default()
    };
    let Some(id) = db.query(&query) else {
        return false;
    };
    add_egui_font_face(fonts, db, id, &name, placement)
}

fn add_egui_font_for_char(fonts: &mut FontDefinitions, db: &fontdb::Database, ch: char) -> bool {
    let Some(face) = egui_symbol_fallback_face(db, ch) else {
        return false;
    };
    let name = format!(
        "bootty-ui-symbol-U{:04X}-{}",
        u32::from(ch),
        face.post_script_name
    );
    add_egui_font_face(fonts, db, face.id, &name, EguiFontPlacement::Last)
}

fn add_egui_font_face(
    fonts: &mut FontDefinitions,
    db: &fontdb::Database,
    id: fontdb::ID,
    name: &str,
    placement: EguiFontPlacement,
) -> bool {
    let name = db
        .face(id)
        .map(|face| format!("bootty-ui-face-{}", face.post_script_name))
        .unwrap_or_else(|| name.to_owned());
    if fonts.font_data.contains_key(&name) {
        return false;
    }
    let Some((bytes, index)) = db.with_face_data(id, |data, index| (data.to_vec(), index)) else {
        return false;
    };

    let mut font_data = FontData::from_owned(bytes);
    font_data.index = index;
    fonts
        .font_data
        .insert(name.clone(), std::sync::Arc::new(font_data));
    for family in [FontFamily::Monospace, FontFamily::Proportional] {
        let entries = fonts.families.entry(family).or_default();
        match placement {
            EguiFontPlacement::First => entries.insert(0, name.clone()),
            EguiFontPlacement::Last => entries.push(name.clone()),
        }
    }
    true
}

fn egui_symbol_fallback_face(db: &fontdb::Database, ch: char) -> Option<&fontdb::FaceInfo> {
    let mut fallback = None;
    for face in db.faces() {
        if !font_face_supports_char(db, face.id, ch) {
            continue;
        }
        if face.monospaced {
            return Some(face);
        }
        fallback.get_or_insert(face);
    }
    fallback
}

fn font_face_supports_char(db: &fontdb::Database, id: fontdb::ID, ch: char) -> bool {
    db.with_face_data(id, |data, index| {
        ttf_parser::Face::parse(data, index).is_ok_and(|face| face.glyph_index(ch).is_some())
    })
    .unwrap_or(false)
}

fn egui_font_name(family: &str) -> String {
    format!("bootty-ui-{family}")
}
