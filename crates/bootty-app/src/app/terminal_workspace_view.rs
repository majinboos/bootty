use std::{
    collections::{HashMap, HashSet},
    sync::OnceLock,
    time::Instant,
};

use eframe::{
    egui::{self, Pos2, Rect, Vec2},
    wgpu,
};

use super::{
    binding_terminal_facts::{TerminalProgress, TerminalProgressState},
    state::AppState,
};
use crate::{
    config::MultiplexerBackendConfig,
    geometry::ViewTransform,
    layout::SplitDirection,
    renderer::{RendererMetrics, TerminalWidget},
    terminal_text::TerminalTextConfig,
};

const MIN_PANE_PX: f32 = 80.0;
const MIN_PANE_DIVIDER_GRAB: f32 = 8.0;
const TERMINAL_PROGRESS_HEIGHT: f32 = 2.0;
const INDETERMINATE_PROGRESS_WIDTH: f32 = 0.25;
const INDETERMINATE_PROGRESS_CYCLE: f64 = 1.5;

pub(super) struct TerminalViewSnapshot {
    pub(super) renderer_metrics: RendererMetrics,
    pub(super) cell_width: f32,
    pub(super) cell_height: f32,
    pub(super) view_transform: ViewTransform,
}

/// Owns the renderer state and interaction lifecycle for one Bootty workspace view.
pub(super) struct TerminalWorkspaceView {
    focused: TerminalWidget,
    inactive: HashMap<String, TerminalWidget>,
    focused_key: Option<String>,
    target_format: Option<wgpu::TextureFormat>,
    text_config: TerminalTextConfig,
    cursor_icon: egui::CursorIcon,
}

impl TerminalWorkspaceView {
    pub(super) fn new(
        target_format: Option<wgpu::TextureFormat>,
        text_config: TerminalTextConfig,
    ) -> Self {
        Self {
            focused: new_widget(target_format, text_config.clone(), egui::CursorIcon::Text),
            inactive: HashMap::new(),
            focused_key: None,
            target_format,
            text_config,
            cursor_icon: egui::CursorIcon::Text,
        }
    }

    pub(super) fn set_text_config(&mut self, text_config: TerminalTextConfig) {
        self.text_config = text_config.clone();
        self.focused.set_text_config(text_config.clone());
        for widget in self.inactive.values_mut() {
            widget.set_text_config(text_config.clone());
        }
    }

    pub(super) fn set_cursor_icon(&mut self, icon: egui::CursorIcon) {
        self.cursor_icon = icon;
        self.focused.set_terminal_cursor_icon(icon);
        for widget in self.inactive.values_mut() {
            widget.set_terminal_cursor_icon(icon);
        }
    }

    pub(super) fn cell_height(&self) -> f32 {
        self.focused.cell_dimensions().1
    }

    pub(super) fn update_input(
        &mut self,
        enabled: bool,
        zoom_delta: f32,
        hover_pos: Option<Pos2>,
        events: &mut Vec<egui::Event>,
    ) -> TerminalViewSnapshot {
        let (cell_width, cell_height) = self.focused.cell_dimensions();
        if enabled && (zoom_delta - 1.0).abs() > f32::EPSILON {
            self.focused.apply_pinch(zoom_delta, hover_pos);
        }
        if enabled && self.focused.is_zoomed() {
            let pan = take_scroll_for_pan(events, cell_height);
            if pan != Vec2::ZERO {
                self.focused.apply_pan(pan);
            }
        }
        TerminalViewSnapshot {
            renderer_metrics: self.focused.metrics(),
            cell_width,
            cell_height,
            view_transform: self.focused.view_transform(),
        }
    }

    pub(super) fn show(
        &mut self,
        state: &mut AppState,
        ui: &mut egui::Ui,
        area: Rect,
        palette: bootty_ui::ThemePalette,
        background: egui::Color32,
        divider_color_override: Option<egui::Color32>,
    ) {
        let native_layout = backend_uses_native_layout_renderer(state.multiplexer_backend());
        let has_terminal = !native_layout
            || state
                .mux()
                .selected_session_anchor()
                .is_some_and(|anchor| anchor.pane_id.is_some());
        ui.painter().rect_filled(area, 0.0, background);
        if !has_terminal {
            self.focused.reset();
            paint_empty_terminal(ui, area, palette);
            return;
        }

        state.record_pane_area(area);
        if native_layout {
            if state.native_multi_pane() {
                self.show_split_panes(state, ui, area, palette, background);
                show_pane_dividers(state, ui, area, palette, divider_color_override);
            } else {
                if let Some(focused) = state.focused_pane() {
                    let widget_key = state.pane_widget_key(&focused);
                    self.focus_pane(&widget_key);
                }
                self.focused
                    .set_transition_key(state.terminal_transition_key());
                let geometry = self.focused.geometry_for_rect(area);
                if let Err(error) = state.resize_native_layout_window(geometry.cols, geometry.rows)
                {
                    state.record_render_error(error);
                }
                self.show_single(
                    state,
                    ui,
                    area,
                    state.config().chrome.pane_corner_radius,
                    background,
                );
            }
        } else {
            self.focused_key = None;
            self.focused
                .set_transition_key(state.terminal_transition_key());
            self.show_single(
                state,
                ui,
                area,
                state.config().chrome.pane_corner_radius,
                background,
            );
        }

        if !state.terminal_focused() {
            let dim = state.config().chrome.unfocused_terminal_dim;
            ui.painter().rect_filled(
                area,
                0.0,
                egui::Color32::from_black_alpha((dim.clamp(0.0, 1.0) * 255.0) as u8),
            );
        }
    }

    fn focus_pane(&mut self, key: &str) {
        if self.focused_key.as_deref() == Some(key) {
            return;
        }
        if self.focused_key.is_none() {
            self.focused_key = Some(key.to_owned());
            return;
        }
        let target_format = self.target_format;
        let text_config = self.text_config.clone();
        let cursor_icon = self.cursor_icon;
        let incoming = self
            .inactive
            .remove(key)
            .unwrap_or_else(|| new_widget(target_format, text_config, cursor_icon));
        let outgoing = std::mem::replace(&mut self.focused, incoming);
        if let Some(old_key) = self.focused_key.replace(key.to_owned()) {
            self.inactive.insert(old_key, outgoing);
        }
    }

    fn show_single(
        &mut self,
        state: &mut AppState,
        ui: &mut egui::Ui,
        rect: Rect,
        corner_radius_px: f32,
        background: egui::Color32,
    ) {
        match self
            .focused
            .show_at_rect(ui, rect, "primary-terminal", state.terminal_mut())
        {
            Ok(output) => {
                if output.viewport_scroll_delta != 0
                    && let Err(error) = state
                        .terminal_mut()
                        .scroll_viewport_delta(output.viewport_scroll_delta)
                {
                    state.record_render_error(error);
                }
                state.record_surface(output.surface);
            }
            Err(error) => state.record_render_error(error),
        }
        paint_pane_corner_masks(ui.painter(), rect, corner_radius_px, background);
        if let Some(progress) = state.current_terminal_progress() {
            paint_terminal_progress(
                ui.painter(),
                rect,
                progress,
                state.ui_theme().palette,
                progress_animation_time(),
            );
        }
    }

    fn show_split_panes(
        &mut self,
        state: &mut AppState,
        ui: &mut egui::Ui,
        area: Rect,
        palette: bootty_ui::ThemePalette,
        background: egui::Color32,
    ) {
        let chrome = &state.config().chrome;
        let gap = chrome.pane_divider_width;
        let border_width = chrome.pane_focus_border_width;
        let border_color = chrome
            .pane_focus_border_color
            .map(crate::theme::config_color32)
            .unwrap_or(palette.accent);
        let corner_radius_px = chrome.pane_corner_radius;
        let inactive_dim = chrome.unfocused_terminal_dim.clamp(0.0, 1.0);
        let rects = state.pane_rects(area, gap);

        if ui.input(|input| input.pointer.primary_pressed())
            && let Some(pos) = ui.input(|input| input.pointer.interact_pos())
            && let Some((pane_id, _)) = rects.iter().find(|(_, rect)| rect.contains(pos))
        {
            state.focus_pane(pane_id);
        }
        let focused = state.focused_pane();
        let focused_widget_key = focused
            .as_deref()
            .map(|pane_id| state.pane_widget_key(pane_id));
        if let Some(focused_widget_key) = &focused_widget_key {
            self.focus_pane(focused_widget_key);
        }

        let pane_geometries: Vec<(String, String, crate::geometry::TerminalGeometry)> = rects
            .iter()
            .map(|(pane_id, rect)| {
                let widget_key = state.pane_widget_key(pane_id);
                let is_focused = focused.as_deref() == Some(pane_id.as_str());
                let geometry = if is_focused {
                    self.focused.geometry_for_rect(*rect)
                } else {
                    let target_format = self.target_format;
                    let text_config = self.text_config.clone();
                    let cursor_icon = self.cursor_icon;
                    let widget = self
                        .inactive
                        .entry(widget_key.clone())
                        .or_insert_with(|| new_widget(target_format, text_config, cursor_icon));
                    widget.geometry_for_rect(*rect)
                };
                (pane_id.clone(), widget_key, geometry)
            })
            .collect();
        if let Some((cols, rows)) = state.pane_terminal_window_size(|pane| {
            pane_geometries
                .iter()
                .find(|(pane_id, _, _)| pane_id.as_str() == pane)
                .map(|(_, _, geometry)| (geometry.cols, geometry.rows))
        }) && let Err(error) = state.resize_native_layout_window(cols, rows)
        {
            state.record_render_error(error);
        }
        let current_ids: HashSet<String> = pane_geometries
            .iter()
            .map(|(_, key, _)| key.clone())
            .collect();
        for (pane_id, rect) in &rects {
            let widget_key = state.pane_widget_key(pane_id);
            let is_focused = focused.as_deref() == Some(pane_id.as_str());
            let result = if is_focused {
                Some(self.focused.show_at_rect(
                    ui,
                    *rect,
                    ("native-pane", &widget_key),
                    state.terminal_mut(),
                ))
            } else {
                let target_format = self.target_format;
                let text_config = self.text_config.clone();
                let cursor_icon = self.cursor_icon;
                let widget = self
                    .inactive
                    .entry(widget_key.clone())
                    .or_insert_with(|| new_widget(target_format, text_config, cursor_icon));
                state.terminal_runtime_for_pane(pane_id).map(|source| {
                    let output =
                        widget.show_at_rect(ui, *rect, ("native-pane", &widget_key), source)?;
                    if output.viewport_scroll_delta != 0 {
                        source.scroll_viewport_delta(output.viewport_scroll_delta)?;
                    }
                    Ok(output)
                })
            };
            match result {
                Some(Ok(output)) if is_focused => {
                    if output.viewport_scroll_delta != 0
                        && let Err(error) = state
                            .terminal_mut()
                            .scroll_viewport_delta(output.viewport_scroll_delta)
                    {
                        state.record_render_error(error);
                    }
                    state.record_surface(output.surface);
                }
                Some(Ok(_)) | None => {}
                Some(Err(error)) => state.record_render_error(error),
            }
            let corner = pane_corner_radius(*rect, corner_radius_px);
            if !is_focused && inactive_dim > 0.0 {
                ui.painter().rect_filled(
                    *rect,
                    corner,
                    egui::Color32::from_black_alpha((inactive_dim * 255.0) as u8),
                );
            }
            paint_pane_corner_masks(ui.painter(), *rect, corner_radius_px, background);
            if let Some(progress) = state.pane_progress(pane_id) {
                paint_terminal_progress(
                    ui.painter(),
                    *rect,
                    progress,
                    palette,
                    progress_animation_time(),
                );
            }
            if is_focused && border_width > 0.0 {
                ui.painter().rect_stroke(
                    *rect,
                    corner,
                    egui::Stroke::new(border_width, border_color),
                    egui::StrokeKind::Inside,
                );
            }
        }
        self.inactive.retain(|key, _| current_ids.contains(key));
    }
}

fn new_widget(
    target_format: Option<wgpu::TextureFormat>,
    text_config: TerminalTextConfig,
    cursor_icon: egui::CursorIcon,
) -> TerminalWidget {
    let mut widget = TerminalWidget::new(target_format).with_text_config(text_config);
    widget.set_terminal_cursor_icon(cursor_icon);
    widget
}

fn backend_uses_native_layout_renderer(backend: MultiplexerBackendConfig) -> bool {
    matches!(
        backend,
        MultiplexerBackendConfig::Native | MultiplexerBackendConfig::Rmux
    )
}

fn pane_corner_radius_px(rect: Rect, px: f32) -> f32 {
    let max = (rect.width().min(rect.height()) / 2.0).max(0.0);
    px.clamp(0.0, max)
}

fn pane_corner_radius(rect: Rect, px: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(pane_corner_radius_px(rect, px).round().clamp(0.0, 255.0) as u8)
}

fn paint_pane_corner_masks(painter: &egui::Painter, rect: Rect, radius: f32, bg: egui::Color32) {
    let r = pane_corner_radius_px(rect, radius).round();
    if r <= 0.5 {
        return;
    }
    let corners = [
        (
            Pos2::new(rect.min.x + r, rect.min.y + r),
            rect.min,
            std::f32::consts::PI,
        ),
        (
            Pos2::new(rect.max.x - r, rect.min.y + r),
            Pos2::new(rect.max.x, rect.min.y),
            std::f32::consts::FRAC_PI_2 * 3.0,
        ),
        (Pos2::new(rect.max.x - r, rect.max.y - r), rect.max, 0.0),
        (
            Pos2::new(rect.min.x + r, rect.max.y - r),
            Pos2::new(rect.min.x, rect.max.y),
            std::f32::consts::FRAC_PI_2,
        ),
    ];
    let steps = 16;
    let mut mesh = egui::epaint::Mesh::default();
    for (center, corner, start) in corners {
        for step in 0..steps {
            let idx = mesh.vertices.len() as u32;
            mesh.colored_vertex(corner, bg);
            for arc_step in [step, step + 1] {
                let angle = start + std::f32::consts::FRAC_PI_2 * (arc_step as f32 / steps as f32);
                mesh.colored_vertex(
                    Pos2::new(center.x + r * angle.cos(), center.y + r * angle.sin()),
                    bg,
                );
            }
            mesh.add_triangle(idx, idx + 1, idx + 2);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

fn terminal_progress_color(
    progress: TerminalProgress,
    palette: bootty_ui::ThemePalette,
) -> egui::Color32 {
    match progress.state {
        TerminalProgressState::Normal | TerminalProgressState::Indeterminate => palette.accent,
        TerminalProgressState::Error => palette.destructive,
        TerminalProgressState::Warning => palette.warning,
    }
}

fn progress_animation_time() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

fn indeterminate_progress_left(track_width: f32, time: f64) -> f32 {
    let segment_width = track_width * INDETERMINATE_PROGRESS_WIDTH;
    let phase = (time / INDETERMINATE_PROGRESS_CYCLE).fract() as f32;
    let travel = 1.0 - (phase * 2.0 - 1.0).abs();
    (track_width - segment_width).max(0.0) * travel
}

pub(super) fn animate_indeterminate_progress(
    window_focused: bool,
    has_indeterminate_progress: bool,
) -> bool {
    window_focused && has_indeterminate_progress
}

fn paint_terminal_progress(
    painter: &egui::Painter,
    rect: Rect,
    progress: TerminalProgress,
    palette: bootty_ui::ThemePalette,
    time: f64,
) {
    let track = Rect::from_min_size(
        rect.min,
        egui::vec2(rect.width(), TERMINAL_PROGRESS_HEIGHT.min(rect.height())),
    );
    painter.rect_filled(track, 0.0, palette.border);
    let (fill_left, fill_width) = if progress.state == TerminalProgressState::Indeterminate {
        let fill_width = track.width() * INDETERMINATE_PROGRESS_WIDTH;
        (
            track.min.x + indeterminate_progress_left(track.width(), time),
            fill_width,
        )
    } else {
        let Some(fraction) = progress.fraction() else {
            return;
        };
        (track.min.x, track.width() * fraction.clamp(0.0, 1.0))
    };
    let fill = Rect::from_min_size(
        Pos2::new(fill_left, track.min.y),
        egui::vec2(fill_width, track.height()),
    );
    painter.rect_filled(fill, 0.0, terminal_progress_color(progress, palette));
}

fn inset_divider_for_radius(rect: Rect, direction: SplitDirection, radius: f32) -> Rect {
    match direction {
        SplitDirection::Right => {
            let inset = radius.clamp(0.0, rect.height() / 2.0);
            Rect::from_min_max(
                Pos2::new(rect.min.x, rect.min.y + inset),
                Pos2::new(rect.max.x, rect.max.y - inset),
            )
        }
        SplitDirection::Down => {
            let inset = radius.clamp(0.0, rect.width() / 2.0);
            Rect::from_min_max(
                Pos2::new(rect.min.x + inset, rect.min.y),
                Pos2::new(rect.max.x - inset, rect.max.y),
            )
        }
    }
}

fn show_pane_dividers(
    state: &mut AppState,
    ui: &mut egui::Ui,
    area: Rect,
    palette: bootty_ui::ThemePalette,
    divider_color_override: Option<egui::Color32>,
) {
    let chrome = &state.config().chrome;
    let gap = chrome.pane_divider_width;
    let corner_radius = chrome.pane_corner_radius;
    let divider_color = divider_color_override.unwrap_or_else(|| {
        chrome
            .pane_divider_color
            .map(crate::theme::config_color32)
            .unwrap_or(palette.mantle)
    });
    let dividers = state.pane_dividers(area, gap);
    for divider in &dividers {
        let direction = divider.direction;
        let handle_rect = match direction {
            SplitDirection::Right => Rect::from_center_size(
                divider.rect.center(),
                egui::vec2(
                    divider.rect.width().max(MIN_PANE_DIVIDER_GRAB),
                    divider.rect.height(),
                ),
            ),
            SplitDirection::Down => Rect::from_center_size(
                divider.rect.center(),
                egui::vec2(
                    divider.rect.width(),
                    divider.rect.height().max(MIN_PANE_DIVIDER_GRAB),
                ),
            ),
        };
        state.register_chrome_handle(handle_rect);
        let visual = inset_divider_for_radius(divider.rect, direction, corner_radius);
        if visual.width() >= 1.0 && visual.height() >= 1.0 {
            ui.painter().rect_filled(visual, 0.0, divider_color);
        }
        let response = egui::Area::new(egui::Id::new((
            "bootty-pane-divider",
            divider.path.as_slice(),
        )))
        .order(egui::Order::Foreground)
        .fixed_pos(handle_rect.min)
        .show(ui.ctx(), |ui| {
            let response = ui.allocate_rect(handle_rect, egui::Sense::drag());
            if response.hovered() || response.dragged() {
                let stroke = egui::Stroke::new(2.0, palette.primary);
                let painter = ui.painter();
                match direction {
                    SplitDirection::Right => {
                        let x = handle_rect.center().x;
                        painter.line_segment(
                            [
                                Pos2::new(x, handle_rect.min.y),
                                Pos2::new(x, handle_rect.max.y),
                            ],
                            stroke,
                        );
                    }
                    SplitDirection::Down => {
                        let y = handle_rect.center().y;
                        painter.line_segment(
                            [
                                Pos2::new(handle_rect.min.x, y),
                                Pos2::new(handle_rect.max.x, y),
                            ],
                            stroke,
                        );
                    }
                }
            }
            response
        })
        .inner;
        if response.hovered() || response.dragged() {
            ui.set_cursor_icon(match direction {
                SplitDirection::Right => egui::CursorIcon::ResizeHorizontal,
                SplitDirection::Down => egui::CursorIcon::ResizeVertical,
            });
        }
        if response.dragged()
            && let Some(pos) = ui.ctx().pointer_interact_pos()
        {
            let extent = match direction {
                SplitDirection::Right => divider.area.width(),
                SplitDirection::Down => divider.area.height(),
            } - gap;
            if extent > 1.0 {
                let min_fraction = (MIN_PANE_PX / extent).clamp(0.05, 0.45);
                state.set_pane_ratio(&divider.path, divider.ratio_at(pos, gap), min_fraction);
            }
        }
    }
}

fn paint_empty_terminal(ui: &egui::Ui, rect: Rect, palette: bootty_ui::ThemePalette) {
    let painter = ui.painter_at(rect);
    let color = palette.muted;
    let galley = crate::ui::keycaps::inline_shortcut_galley_from_painter(
        &painter,
        palette,
        crate::ui::keycaps::InlineShortcut {
            prefix: "No open tabs - press ",
            trigger: crate::platform::new_tab_shortcut_trigger(),
            suffix: " to open one",
        },
        color,
        rect.width(),
        13.0,
    );
    painter.galley(rect.center() - galley.size() * 0.5, galley, color);
}

fn take_scroll_for_pan(events: &mut Vec<egui::Event>, line_height: f32) -> Vec2 {
    let mut pan = Vec2::ZERO;
    events.retain(|event| {
        let egui::Event::MouseWheel {
            unit,
            delta,
            modifiers,
            ..
        } = event
        else {
            return true;
        };
        if modifiers.alt {
            return true;
        }
        let scale = match unit {
            egui::MouseWheelUnit::Point => 1.0,
            egui::MouseWheelUnit::Line => line_height,
            egui::MouseWheelUnit::Page => line_height * 20.0,
        };
        pan += *delta * scale;
        false
    });
    pan
}
