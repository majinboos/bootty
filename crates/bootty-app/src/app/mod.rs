mod state;

use std::{sync::mpsc, time::Instant};

use anyhow::Result;
use eframe::egui::{
    self, FontData, FontDefinitions, FontFamily, Pos2, Rect, TextureHandle, UiBuilder,
};

pub use state::{AppEffect, AppState, FrameInputs, ViewportSnapshot};

use crate::{
    config::BoottyConfig,
    direct_input::{DirectKeyInput, ModifierSideState, suppress_egui_events_for_direct_input},
    mux::config::selected_backend,
    renderer::TerminalWidget,
    theme::theme_palette_from_config,
    ui::chrome::{self, SidebarModel, StatusBarModel},
};

pub struct BoottyApp {
    state: AppState,
    terminal_widget: TerminalWidget,
    app_icon_texture: Option<TextureHandle>,
}

impl BoottyApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Result<Self> {
        Self::new_with_config(cc, BoottyConfig::default())
    }

    pub fn new_with_config(cc: &eframe::CreationContext<'_>, config: BoottyConfig) -> Result<Self> {
        Self::new_inner(cc, config, None, None)
    }

    pub fn new_with_direct_input(
        cc: &eframe::CreationContext<'_>,
        config: BoottyConfig,
        direct_input_rx: mpsc::Receiver<DirectKeyInput>,
        modifier_side_rx: mpsc::Receiver<ModifierSideState>,
    ) -> Result<Self> {
        Self::new_inner(cc, config, Some(direct_input_rx), Some(modifier_side_rx))
    }

    fn new_inner(
        cc: &eframe::CreationContext<'_>,
        config: BoottyConfig,
        direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
        modifier_side_rx: Option<mpsc::Receiver<ModifierSideState>>,
    ) -> Result<Self> {
        configure_egui_fonts(&cc.egui_ctx, &config.font.family);
        let repaint_ctx = cc.egui_ctx.clone();
        let repaint: crate::mux::RepaintHandle =
            std::sync::Arc::new(move || repaint_ctx.request_repaint());
        let text_config = config.font.terminal_text_config();
        let terminal_widget = TerminalWidget::new(
            cc.wgpu_render_state
                .as_ref()
                .map(|render_state| render_state.target_format),
        )
        .with_text_config(text_config);

        Ok(Self {
            state: AppState::new(config, repaint, direct_input_rx, modifier_side_rx)?,
            terminal_widget,
            app_icon_texture: None,
        })
    }

    fn apply_effects(&mut self, ctx: &egui::Context, effects: Vec<AppEffect>) {
        for effect in effects {
            match effect {
                AppEffect::CloseWindow => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
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
                AppEffect::RepaintAfter(after) => ctx.request_repaint_after(after),
                AppEffect::SetTerminalTextConfig(text_config) => {
                    self.terminal_widget.set_text_config(text_config);
                }
            }
        }
    }

    fn show_fixed_layout(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        let palette = theme_palette_from_config(self.state.config());
        let chrome_config = &self.state.config().chrome;
        let sidebar = chrome_config.sidebar;
        let status_bar = chrome_config.status_bar;
        let configured_sidebar_width = chrome_config.sidebar_width;
        let status_height_config = chrome_config.status_height;
        let chrome_gap = chrome_config.gap;
        let fullscreen_chrome = self.state.macos_non_native_fullscreen_active()
            || ui
                .ctx()
                .input(|input| input.viewport().fullscreen.unwrap_or(false));
        let sidebar_width = if sidebar {
            configured_sidebar_width
        } else {
            0.0
        };
        let gap = if sidebar && sidebar_width > 0.0 && !fullscreen_chrome {
            chrome_gap
        } else {
            0.0
        };
        let status_height = if status_bar {
            status_height_config
        } else {
            0.0
        };
        let show_window_tabs = matches!(
            self.state.config().multiplexer.backend,
            crate::config::MultiplexerBackendConfig::Rmux
                | crate::config::MultiplexerBackendConfig::Native
        ) && !self.state.mux().selected_session_windows().is_empty();
        let window_tabs_height = if show_window_tabs { 34.0 } else { 0.0 };
        let sidebar_rect = if sidebar {
            Rect::from_min_size(
                rect.min,
                egui::vec2(sidebar_width.min(rect.width()), rect.height()),
            )
        } else {
            Rect::from_min_size(rect.min, egui::vec2(0.0, rect.height()))
        };
        let right_rect = Rect::from_min_max(
            Pos2::new((sidebar_rect.max.x + gap).min(rect.max.x), rect.min.y),
            rect.max,
        );
        let status_rect = Rect::from_min_size(
            right_rect.min,
            egui::vec2(right_rect.width(), status_height.min(right_rect.height())),
        );
        let window_tabs_rect = Rect::from_min_max(
            Pos2::new(right_rect.min.x, status_rect.max.y),
            Pos2::new(
                right_rect.max.x,
                (status_rect.max.y + window_tabs_height).min(right_rect.max.y),
            ),
        );
        let terminal_rect = Rect::from_min_max(
            Pos2::new(right_rect.min.x, window_tabs_rect.max.y),
            right_rect.max,
        );

        if sidebar {
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(sidebar_rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
                |ui| {
                    let title_visible = self.state.config().window.custom_chrome_title_visible();
                    let reserve_titlebar_buttons = self
                        .state
                        .config()
                        .window
                        .reserves_macos_titlebar_button_area();
                    let top_inset = if fullscreen_chrome && !title_visible {
                        28.0
                    } else {
                        0.0
                    };
                    let title_icon = title_visible.then(|| {
                        chrome::load_app_icon_texture(ui.ctx(), &mut self.app_icon_texture)
                    });
                    if let Some(event) = chrome::show_sidebar(
                        ui,
                        palette,
                        sidebar_rect.height(),
                        SidebarModel {
                            sessions: self.state.mux().sessions(),
                            selected_session: self.state.mux().selected_session(),
                            metadata: self.state.sidebar_metadata(),
                            title_visible,
                            reserve_titlebar_buttons,
                            title_icon: title_icon.as_ref(),
                            top_inset,
                            border_visible: !fullscreen_chrome,
                            separator_visible: !fullscreen_chrome,
                            focused: self.state.sidebar_focused(),
                            hovered_session: self.state.sidebar_hovered_session(),
                            unfocused_dim: self.state.config().chrome.unfocused_sidebar_dim,
                        },
                    ) {
                        match event {
                            chrome::SidebarEvent::ActivateSession(session_id) => {
                                self.state.activate_session_from_ui(&session_id);
                            }
                            chrome::SidebarEvent::Reorder { source, before } => {
                                self.state
                                    .reorder_session_before(&source, before.as_deref());
                            }
                        }
                    }
                },
            );
        }

        if status_bar {
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(status_rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                |ui| {
                    chrome::show_status_bar(
                        ui,
                        palette,
                        StatusBarModel {
                            backend: selected_backend(&self.state.config().multiplexer),
                            selected_session_name: chrome::selected_session_name(
                                self.state.mux().sessions(),
                                self.state.mux().selected_session(),
                            ),
                            metrics: self.state.status_metrics(),
                            last_error: self.state.last_error(),
                        },
                    );
                },
            );
        }

        if show_window_tabs {
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(window_tabs_rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                |ui| {
                    if let Some(window_id) = chrome::show_window_tabs(
                        ui,
                        palette,
                        chrome::WindowTabsModel {
                            windows: self.state.mux().selected_session_windows(),
                            selected_window: self.state.mux().selected_window(),
                        },
                    ) && let Some(session_id) =
                        self.state.mux().selected_session().map(str::to_owned)
                    {
                        self.state.activate_window_from_ui(&session_id, &window_id);
                    }
                },
            );
        }
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(terminal_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
            |ui| {
                match self.terminal_widget.show(ui, self.state.terminal_mut()) {
                    Ok(surface) => self.state.record_surface(surface),
                    Err(error) => self.state.record_render_error(error),
                }
                if !self.state.terminal_focused() {
                    let dim = self.state.config().chrome.unfocused_terminal_dim;
                    ui.painter().rect_filled(
                        terminal_rect,
                        0.0,
                        egui::Color32::from_black_alpha((dim.clamp(0.0, 1.0) * 255.0) as u8),
                    );
                }
            },
        );
    }

    fn show_new_mux_session_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.state.take_dialog() else {
            return;
        };
        let event = dialog.show(ctx, self.state.ui_theme());
        self.state.apply_picker_event(dialog, event);
    }

    fn show_session_picker_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.state.take_session_picker_dialog() else {
            return;
        };
        let sessions = self.state.mux().sessions().to_vec();
        let selected_session = self.state.mux().selected_session().map(str::to_owned);
        let event = dialog.show(
            ctx,
            self.state.ui_theme(),
            &sessions,
            selected_session.as_deref(),
        );
        self.state.apply_session_picker_event(dialog, event);
    }
}

impl eframe::App for BoottyApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.state.drain_direct_input();
        if self.state.direct_input_suppresses_egui_events() {
            suppress_egui_events_for_direct_input(
                &mut raw_input.events,
                self.state.pending_direct_input(),
            );
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let (events, modifiers, hover_pos, pressed_mouse_button, stable_dt, viewport) =
            ctx.input(|input| {
                (
                    input.events.clone(),
                    input.modifiers,
                    input.pointer.hover_pos(),
                    crate::input::pressed_mouse_button_from_egui(&input.pointer),
                    input.stable_dt,
                    ViewportSnapshot {
                        fullscreen: input.viewport().fullscreen.unwrap_or(false),
                        maximized: input.viewport().maximized.unwrap_or(false),
                        content_height: input.content_rect().height(),
                    },
                )
            });
        let inputs = FrameInputs {
            now: Instant::now(),
            stable_dt_ms: stable_dt * 1000.0,
            events,
            modifiers,
            hover_pos,
            pressed_mouse_button,
            viewport,
            renderer_metrics: self.terminal_widget.metrics(),
        };
        let effects = self.state.update_frame(inputs);
        self.apply_effects(ctx, effects);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let palette = self.state.ui_theme().palette;
        egui::Frame::NONE.fill(palette.mantle).show(ui, |ui| {
            self.show_fixed_layout(ui);
        });
        self.show_new_mux_session_dialog(ui.ctx());
        self.show_session_picker_dialog(ui.ctx());
    }
}

fn configure_egui_fonts(ctx: &egui::Context, families: &[String]) {
    let db = bootty_render::font_database::system_font_database();
    let mut fonts = FontDefinitions::default();
    for family in families.iter().rev() {
        let query_families = [fontdb::Family::Name(family)];
        let query = fontdb::Query {
            families: &query_families,
            ..fontdb::Query::default()
        };
        let Some(id) = db.query(&query) else {
            continue;
        };
        let Some((bytes, index)) = db.with_face_data(id, |data, index| (data.to_vec(), index))
        else {
            continue;
        };
        let name = format!("bootty-ui-{family}");
        let mut font_data = FontData::from_owned(bytes);
        font_data.index = index;
        fonts
            .font_data
            .insert(name.clone(), std::sync::Arc::new(font_data));
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .insert(0, name.clone());
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, name);
    }

    ctx.set_fonts(fonts);
}
